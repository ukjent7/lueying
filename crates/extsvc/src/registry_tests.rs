//! 服务缝的回归网:IDL 与方法白名单一致、阻塞调用、审计与取消。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lueying_extmeta::Service;

use crate::{AuditOutcome, AuditSink, Done, Provider, Registry, WIT, methods};

const WAIT: Duration = Duration::from_secs(2);

/// 把结果原样回传的假提供者;记录收到的调用供断言。
struct Fake {
    ok: bool,
    payload: String,
    seen: std::sync::Mutex<Vec<(String, String)>>,
}

impl Provider for Fake {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
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

#[test]
fn call_rejects_unknown_service_and_method() {
    let registry = Registry::new();
    assert!(
        registry
            .call("", "shot", "shot", "", WAIT)
            .err()
            .unwrap()
            .contains("未知服务")
    );
    assert!(
        registry
            .call("", "capture", "record", "", WAIT)
            .err()
            .unwrap()
            .contains("没有方法")
    );
    assert!(
        registry
            .call("", "store", "post", "", WAIT)
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
            .call("", "capture", "shot", "", WAIT)
            .err()
            .unwrap()
            .contains("没有提供者")
    );
}

#[test]
fn provider_receives_method_and_result_reaches_caller() {
    let registry = Registry::new();
    let provider = fake(
        true,
        r#"{"file":"C:\\tmp\\shot.bmp","width":800,"height":200}"#,
    );
    registry.set(Service::Capture, provider.clone());
    assert!(registry.has(Service::Capture));
    let result = registry
        .call("", "capture", "shot", r#"{"region":null}"#, WAIT)
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
    assert_eq!(
        result,
        r#"{"file":"C:\\tmp\\shot.bmp","width":800,"height":200}"#
    );
}

#[test]
fn provider_error_reaches_caller() {
    let registry = Registry::new();
    registry.set(Service::Hotkey, fake(false, "热键已注册: paste"));
    let error = registry
        .call("", "hotkey", "register", r#"{"id":"paste"}"#, WAIT)
        .unwrap_err();
    assert_eq!(error, "热键已注册: paste");
}

/// 完成可以来自任意线程;调用方阻塞等到结果。
#[test]
fn completion_from_another_thread() {
    let registry = Registry::new();
    struct Threaded;
    impl Provider for Threaded {
        fn call(
            &self,
            _caller: &str,
            _method: &str,
            _args: &str,
            done: Done,
        ) -> Result<(), String> {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(10));
                done.complete(Ok(r#"{"delivered":true}"#.to_string()));
            });
            Ok(())
        }
    }
    registry.set(Service::Output, Arc::new(Threaded));
    let result = registry
        .call("", "output", "deliver", r#"{"text":"你好"}"#, WAIT)
        .unwrap();
    assert_eq!(result, r#"{"delivered":true}"#);
}

/// 同服务重复装是替换,后装的接活。
#[test]
fn set_replaces_provider() {
    let registry = Registry::new();
    registry.set(Service::Ocr, fake(true, r#"{"text":"一"}"#));
    registry.set(Service::Ocr, fake(true, r#"{"text":"二"}"#));
    let result = registry
        .call("", "ocr", "recognize", r#"{"file":"a"}"#, WAIT)
        .unwrap();
    assert_eq!(result, r#"{"text":"二"}"#);
}

/// 审计接收端:只记调用方、服务、方法与结果。
struct Recorder {
    lines: std::sync::Mutex<Vec<String>>,
}

impl AuditSink for Recorder {
    fn record(&self, caller: &str, service: &str, method: &str, outcome: AuditOutcome, note: &str) {
        self.lines
            .lock()
            .unwrap()
            .push(format!("{caller} {service}.{method} {outcome:?} {note}"));
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
    registry
        .call("shot-ocr", "capture", "shot", "{}", WAIT)
        .unwrap();
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
        .call("shot-ocr", "capture", "shot", "{}", WAIT)
        .unwrap_err();
    assert!(error.contains("隐私字段"));
    assert!(provider.seen.lock().unwrap().is_empty());
    assert!(
        recorder.lines.lock().unwrap()[0].contains("Denied"),
        "拒绝也要进审计"
    );

    private.store(false, Ordering::Relaxed);
    registry
        .call("shot-ocr", "capture", "shot", "{}", WAIT)
        .unwrap();
    assert_eq!(provider.seen.lock().unwrap().len(), 1);
}

/// 提供者不回传时按超时返回,不把扩展线程永久挂住。
#[test]
fn timeout_returns_error() {
    struct Silent;
    impl Provider for Silent {
        fn call(
            &self,
            _caller: &str,
            _method: &str,
            _args: &str,
            _done: Done,
        ) -> Result<(), String> {
            Ok(())
        }
    }
    let registry = Registry::new();
    registry.set(Service::Ocr, Arc::new(Silent));
    let error = registry
        .call("", "ocr", "recognize", "{}", Duration::from_millis(60))
        .unwrap_err();
    assert!(error.contains("超时"), "{error}");
}

/// 取消标志置位后调用在下一轮检查时返回。
#[test]
fn cancellation_returns_error() {
    struct Silent;
    impl Provider for Silent {
        fn call(
            &self,
            _caller: &str,
            _method: &str,
            _args: &str,
            _done: Done,
        ) -> Result<(), String> {
            Ok(())
        }
    }
    let registry = Arc::new(Registry::new());
    registry.set(Service::Panel, Arc::new(Silent));
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_registry = Arc::clone(&registry);
    let worker_cancel = Arc::clone(&cancel);
    let handle = std::thread::spawn(move || {
        worker_registry.call_cancellable(
            "",
            "panel",
            "show",
            "{}",
            Duration::from_secs(10),
            Some(&worker_cancel),
        )
    });
    std::thread::sleep(Duration::from_millis(20));
    cancel.store(true, Ordering::Relaxed);
    let error = handle.join().unwrap().unwrap_err();
    assert!(error.contains("取消"), "{error}");
}
