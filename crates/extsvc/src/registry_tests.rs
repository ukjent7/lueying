//! 服务缝的回归网:IDL 与方法白名单一致、调用路由与完成队列。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lueying_extmeta::Service;

use crate::{AuditOutcome, AuditSink, Done, Provider, Registry, WIT, methods};

/// 把结果原样回传的假提供者;记录收到的调用供断言。
struct Fake {
    ok: bool,
    payload: String,
    seen: std::sync::Mutex<Vec<(String, String)>>,
}

impl Provider for Fake {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        self.seen
            .lock()
            .unwrap()
            .push((method.to_string(), args.to_string()));
        let result = if self.ok {
            Ok(self.payload.clone())
        } else {
            Err(self.payload.clone())
        };
        done.complete(result);
        Ok(())
    }
}

fn fake(ok: bool, payload: &str) -> Arc<Fake> {
    Arc::new(Fake {
        ok,
        payload: payload.to_string(),
        seen: std::sync::Mutex::new(Vec::new()),
    })
}

/// IDL 里每个开放方法都得有 `名字: func` 一行;改白名单忘了改 IDL 就红。
#[test]
fn wit_lists_every_open_method() {
    for service in Service::ALL {
        for method in methods(service) {
            assert!(
                WIT.contains(&format!("{method}: func")),
                "lueying.wit 缺 {} 的方法 {method}",
                service.key()
            );
        }
    }
}

/// 没开放的接口(剪贴板/网络/存储)没有方法,声明了也调不到。
#[test]
fn closed_services_have_no_methods() {
    for service in [Service::Clipboard, Service::Net, Service::Store] {
        assert!(methods(service).is_empty(), "{} 应该没开放", service.key());
    }
}

#[test]
fn call_rejects_unknown_service_and_method() {
    let registry = Registry::new();
    assert!(registry.call("拍摄", "shot", "", 1).is_err());
    assert!(
        registry
            .call("capture", "record", "", 1)
            .err()
            .unwrap()
            .contains("没有方法")
    );
    assert!(
        registry
            .call("net", "fetch", "", 1)
            .err()
            .unwrap()
            .contains("没有方法")
    );
}

#[test]
fn call_without_provider_is_rejected() {
    let registry = Registry::new();
    assert!(
        registry
            .call("capture", "shot", "", 1)
            .err()
            .unwrap()
            .contains("没有提供者")
    );
}

#[test]
fn provider_receives_method_and_completes_queue() {
    let registry = Registry::new();
    let provider = fake(
        true,
        r#"{"file":"C:\\tmp\\shot.bmp","width":800,"height":200}"#,
    );
    registry.set(Service::Capture, provider.clone());
    assert!(registry.has(Service::Capture));
    registry
        .call("capture", "shot", r#"{"region":null}"#, 7)
        .unwrap();
    assert_eq!(
        provider
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|(method, args)| (method.as_str(), args.as_str()))
            .collect::<Vec<_>>(),
        vec![("shot", r#"{"region":null}"#)]
    );
    let completions = registry.take_completions();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].token, 7);
    assert_eq!(
        completions[0].result.as_deref(),
        Ok(r#"{"file":"C:\\tmp\\shot.bmp","width":800,"height":200}"#)
    );
    assert!(!registry.has_completions());
}

#[test]
fn provider_error_text_reaches_queue() {
    let registry = Registry::new();
    registry.set(Service::Hotkey, fake(false, "热键已注册: paste"));
    registry
        .call("hotkey", "register", r#"{"id":"paste"}"#, 3)
        .unwrap();
    let completions = registry.take_completions();
    assert_eq!(completions[0].result, Err("热键已注册: paste".to_string()));
}

/// 完成可以来自任意线程;泵任务只管抽队列。
#[test]
fn completion_from_another_thread() {
    let registry = Registry::new();
    struct Threaded;
    impl Provider for Threaded {
        fn call(&self, _method: &str, _args: &str, done: Done) -> Result<(), String> {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(10));
                done.complete(Ok(r#"{"delivered":true}"#.to_string()));
            });
            Ok(())
        }
    }
    registry.set(Service::Output, Arc::new(Threaded));
    registry
        .call("output", "deliver", r#"{"text":"你好"}"#, 9)
        .unwrap();
    assert!(!registry.has_completions());
    let completions = registry.take_completions();
    assert!(completions.is_empty());
    std::thread::sleep(std::time::Duration::from_millis(50));
    let completions = registry.take_completions();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].token, 9);
}

/// 同服务重复装是替换,后装的接活。
#[test]
fn set_replaces_provider() {
    let registry = Registry::new();
    registry.set(Service::Ocr, fake(true, r#"{"text":"一"}"#));
    registry.set(Service::Ocr, fake(true, r#"{"text":"二"}"#));
    registry
        .call("ocr", "recognize", r#"{"file":"a"}"#, 1)
        .unwrap();
    let completions = registry.take_completions();
    assert_eq!(completions[0].result.as_deref(), Ok(r#"{"text":"二"}"#));
}

/// 审计接收端:只记调用方、服务、方法与结果。
struct Recorder {
    lines: std::sync::Mutex<Vec<String>>,
}

impl AuditSink for Recorder {
    fn record(&self, caller: &str, service: &str, method: &str, outcome: AuditOutcome, note: &str) {
        self.lines.lock().unwrap().push(format!(
            "{caller} {service}.{method} {outcome:?} {note}"
        ));
    }
}

#[test]
fn calls_are_audited_with_caller() {
    let registry = Registry::new();
    registry.set(Service::Capture, fake(true, "{}"));
    let recorder = Arc::new(Recorder {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    registry.set_audit(recorder.clone());
    registry.call_as("shot-ocr", "capture", "shot", "{}", 1).unwrap();
    let lines = recorder.lines.lock().unwrap();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with("shot-ocr capture.shot Allowed"));
}

#[test]
fn privacy_blocks_before_provider_and_is_audited() {
    let registry = Registry::new();
    let provider = fake(true, "{}");
    registry.set(Service::Capture, provider.clone());
    let recorder = Arc::new(Recorder {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    registry.set_audit(recorder.clone());
    let private = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&private);
    registry.set_privacy(Arc::new(move || flag.load(Ordering::Relaxed)));

    let error = registry
        .call_as("shot-ocr", "capture", "shot", "{}", 1)
        .unwrap_err();
    assert!(error.contains("隐私字段"));
    assert!(provider.seen.lock().unwrap().is_empty());
    assert!(
        recorder.lines.lock().unwrap()[0].contains("Denied"),
        "拒绝也要进审计"
    );

    private.store(false, Ordering::Relaxed);
    registry
        .call_as("shot-ocr", "capture", "shot", "{}", 2)
        .unwrap();
    assert_eq!(provider.seen.lock().unwrap().len(), 1);
}
