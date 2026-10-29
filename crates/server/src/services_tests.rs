//! 服务提供者的 Windows 集成验证:真 GDI 截图、真 WinRT 识别、热键语义。
//!
//! CI runner 可能缺 OCR 语言包:识别只接受确定性的「没有可用引擎」失败,
//! 其余错误都算红。截图与识别走真实平台路径,这部分是 T3 每服务
//! Windows E2E 的落点;文件在验证后删掉。

use std::sync::Arc;
use std::time::{Duration, Instant};

use lueying_extjs::{Completion, Registry};
use lueying_extmeta::Service;

use crate::services::{
    CaptureProvider, HotkeyProvider, NotifyProvider, OcrProvider, OutputProvider, PanelProvider,
};

/// 等一个完成结果;提供者都是异步回传。
fn wait_completion(registry: &Registry, timeout: Duration) -> Option<Completion> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(completion) = registry.take_completions().into_iter().next() {
            return Some(completion);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

#[test]
fn region_parsing() {
    assert!(super::parse_region("").unwrap().is_none());
    assert!(super::parse_region("null").unwrap().is_none());
    assert!(super::parse_region("{}").unwrap().is_none());
    assert!(
        super::parse_region(r#"{"region":{"x":1,"y":2}}"#)
            .err()
            .unwrap()
            .contains("width")
    );
    let region = super::parse_region(r#"{"region":{"x":10,"y":20,"width":300,"height":200}}"#)
        .unwrap()
        .unwrap();
    assert_eq!(
        (region.x, region.y, region.width, region.height),
        (10, 20, 300, 200)
    );
}

#[test]
fn capture_shots_real_screen() {
    let registry = Registry::new();
    registry.set(Service::Capture, Arc::new(CaptureProvider));
    registry.call("capture", "shot", "", 1).unwrap();
    let completion = wait_completion(&registry, Duration::from_secs(30)).expect("截图未完成");
    let result = completion.result.expect("截图失败");
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    let file = parsed["file"].as_str().unwrap();
    let metadata = std::fs::metadata(file).unwrap();
    assert!(metadata.len() > 54, "截图文件太小: {metadata:?}");
    assert!(parsed["width"].as_i64().unwrap() > 0);
    assert!(parsed["height"].as_i64().unwrap() > 0);
    let _ = std::fs::remove_file(file);
}

#[test]
fn ocr_recognizes_captured_shot() {
    let registry = Registry::new();
    registry.set(Service::Capture, Arc::new(CaptureProvider));
    registry.set(Service::Ocr, Arc::new(OcrProvider));
    registry.call("capture", "shot", "", 1).unwrap();
    let shot = wait_completion(&registry, Duration::from_secs(30)).expect("截图未完成");
    let file: String = {
        let parsed: serde_json::Value =
            serde_json::from_str(&shot.result.expect("截图失败")).unwrap();
        parsed["file"].as_str().unwrap().to_string()
    };
    let args = serde_json::json!({ "file": file }).to_string();
    registry.call("ocr", "recognize", &args, 2).unwrap();
    let completion = wait_completion(&registry, Duration::from_secs(60)).expect("识别未完成");
    match completion.result {
        Ok(result) => {
            let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
            assert!(parsed["text"].is_string());
            assert!(parsed["lang"].as_str().is_some_and(|lang| !lang.is_empty()));
        }
        Err(error) => assert!(
            error.contains("没有可用 OCR 引擎"),
            "识别失败原因不确定: {error}"
        ),
    }
    let _ = std::fs::remove_file(file);
}

#[test]
fn hotkey_register_conflict_unregister() {
    let registry = Registry::new();
    registry.set(Service::Hotkey, Arc::new(HotkeyProvider::default()));
    let register = |registry: &Registry, id: &str| {
        registry
            .call(
                "hotkey",
                "register",
                &format!(r#"{{"id":"{id}","modifiers":["ctrl","alt"],"key":"K"}}"#),
                0,
            )
            .unwrap();
        wait_completion(registry, Duration::from_secs(5)).expect("热键未回传")
    };
    let first = register(&registry, "paste");
    assert!(first.result.is_ok(), "{first:?}");
    let second = register(&registry, "paste");
    assert_eq!(second.result.as_deref(), Err("热键已注册: paste"));
    registry
        .call("hotkey", "unregister", r#"{"id":"paste"}"#, 0)
        .unwrap();
    let released = wait_completion(&registry, Duration::from_secs(5)).expect("注销未回传");
    assert!(released.result.is_ok(), "{released:?}");
    let third = register(&registry, "paste");
    assert!(third.result.is_ok(), "{third:?}");
}

#[test]
fn notify_ok_and_stubbed_services_report() {
    let registry = Registry::new();
    registry.set(Service::Notify, Arc::new(NotifyProvider));
    registry.set(Service::Output, Arc::new(OutputProvider));
    registry.set(Service::Panel, Arc::new(PanelProvider));
    registry
        .call("notify", "show", r#"{"title":"标题","body":"正文"}"#, 1)
        .unwrap();
    let completion = wait_completion(&registry, Duration::from_secs(5)).expect("通知未回传");
    assert_eq!(completion.result.as_deref(), Ok("{}"));

    registry
        .call("output", "deliver", r#"{"text":"你好"}"#, 2)
        .unwrap();
    let completion = wait_completion(&registry, Duration::from_secs(5)).expect("output 未回传");
    assert_eq!(
        completion.result.as_deref(),
        Err("output 提交链路在 T6 接线")
    );

    registry
        .call("panel", "show", r#"{"title":"板","items":["甲"]}"#, 3)
        .unwrap();
    let completion = wait_completion(&registry, Duration::from_secs(5)).expect("panel 未回传");
    assert_eq!(completion.result.as_deref(), Err("面板在 T4 落地"));
}
