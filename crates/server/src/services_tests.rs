//! 服务提供者的 Windows 集成验证:真 GDI 截图、真 WinRT 识别、热键语义。
//!
//! CI runner 可能缺 OCR 语言包:识别只接受确定性的「没有可用引擎」失败,
//! 其余错误都算红。截图与识别走真实平台路径,这部分是 T3 每服务
//! Windows E2E 的落点;文件在验证后删掉。

use std::sync::Arc;
use std::time::{Duration, Instant};

use lueying_extjs::{Completion, Registry, Service};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
    VK_SPACE,
};

use crate::services::{
    CaptureProvider, HotkeyProvider, NotifyProvider, OcrProvider, OutputProvider, PanelProvider,
    StoreProvider,
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
    registry.set(Service::Ocr, Arc::new(OcrProvider::new()));
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
    assert_eq!(
        second.result.as_deref().map_err(String::as_str),
        Err("热键已注册: paste")
    );
    registry
        .call("hotkey", "unregister", r#"{"id":"paste"}"#, 0)
        .unwrap();
    let released = wait_completion(&registry, Duration::from_secs(5)).expect("注销未回传");
    assert!(released.result.is_ok(), "{released:?}");
    let third = register(&registry, "paste");
    assert!(third.result.is_ok(), "{third:?}");
}

/// 真实注入一次 Ctrl+Alt+<key>;WM_HOTKEY 投到注册线程的队列,由 pump 收取。
fn send_combo(key: VIRTUAL_KEY) {
    let press = |vk: VIRTUAL_KEY, up: bool| {
        let mut input = INPUT::default();
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki.wVk = vk;
        if up {
            input.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
        }
        input
    };
    let inputs = [
        press(VK_CONTROL, false),
        press(VK_MENU, false),
        press(key, false),
        press(key, true),
        press(VK_MENU, true),
        press(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    assert_eq!(sent as usize, inputs.len(), "SendInput 被拦截");
}

/// 真实注入一次 Ctrl+Alt+Shift+Space;命令面板的默认总热键。
fn send_panel_combo() {
    let press = |vk: VIRTUAL_KEY, up: bool| {
        let mut input = INPUT::default();
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki.wVk = vk;
        if up {
            input.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
        }
        input
    };
    let inputs = [
        press(VK_CONTROL, false),
        press(VK_MENU, false),
        press(VK_SHIFT, false),
        press(VK_SPACE, false),
        press(VK_SPACE, true),
        press(VK_SHIFT, true),
        press(VK_MENU, true),
        press(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    assert_eq!(sent as usize, inputs.len(), "SendInput 被拦截");
}

#[test]
fn hotkey_user_binding_fires() {
    let hotkey = HotkeyProvider::default();
    hotkey.bind(0, "Ctrl+Alt+J").expect("绑定注册失败");
    send_combo(VIRTUAL_KEY(0x4A)); // J
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(hotkey.pump(), vec![0], "触发行号不符");
    hotkey.reset();
    hotkey.bind(1, "Ctrl+Alt+J").expect("reset 后应可重注册");
    hotkey.reset();
}

#[test]
fn hotkey_panel_binding_fires_and_is_taken_once() {
    let hotkey = HotkeyProvider::default();
    assert!(hotkey.bind_panel("Ctrl+O").is_err(), "单修饰键要拒");
    assert!(hotkey.bind_panel("Ctrl+Alt+Del").is_err(), "保留组合要拒");
    hotkey
        .bind_panel("Ctrl+Alt+Shift+Space")
        .expect("面板热键注册失败");
    send_panel_combo();
    std::thread::sleep(Duration::from_millis(300));
    assert!(hotkey.pump().is_empty(), "面板热键不是用户绑定");
    assert!(hotkey.take_panel(), "面板热键没有触发");
    assert!(!hotkey.take_panel(), "一次触发只能取走一次");
    hotkey.unbind_panel();
    hotkey.reset();
}

#[test]
fn hotkey_bind_rejects_bad_and_conflicting_combos() {
    let hotkey = HotkeyProvider::default();
    assert!(hotkey.bind(0, "Ctrl+O").is_err(), "单修饰键要拒");
    assert!(hotkey.bind(0, "Ctrl+Alt+Del").is_err(), "保留组合要拒");
    hotkey.bind(0, "Ctrl+Alt+H").expect("绑定注册失败");
    // 同组合再注册:OS 层冲突,错误文本可读。
    let error = hotkey.bind(1, "ctrl+alt+h").expect_err("应冲突");
    assert!(error.contains("热键注册失败"), "{error}");
    hotkey.reset();
    hotkey.bind(2, "Ctrl+Alt+H").expect("reset 后应可重注册");
    hotkey.reset();
}

#[test]
fn hotkey_press_resolves_on_fire() {
    let registry = Registry::new();
    let hotkey = Arc::new(HotkeyProvider::default());
    // `set` 收 `Arc<dyn Provider>`;先复制成具体类型,参数位置再做 trait 对象强转。
    let provider: Arc<HotkeyProvider> = Arc::clone(&hotkey);
    registry.set(Service::Hotkey, provider);
    registry
        .call(
            "hotkey",
            "register",
            r#"{"id":"paste","modifiers":["ctrl","alt"],"key":"N"}"#,
            1,
        )
        .unwrap();
    let registered = wait_completion(&registry, Duration::from_secs(5)).expect("注册未回传");
    assert!(registered.result.is_ok(), "{registered:?}");
    registry
        .call("hotkey", "press", r#"{"id":"ghost"}"#, 2)
        .unwrap();
    let missing = wait_completion(&registry, Duration::from_secs(5)).expect("未回传");
    assert_eq!(
        missing.result.as_deref().map_err(String::as_str),
        Err("热键未注册: ghost")
    );
    registry
        .call("hotkey", "press", r#"{"id":"paste"}"#, 3)
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        registry.take_completions().is_empty(),
        "press 不能在触发前回传"
    );
    send_combo(VIRTUAL_KEY(0x4E)); // N
    std::thread::sleep(Duration::from_millis(300));
    hotkey.pump(); // 扩展线程的循环节拍在测试里由本线程代收
    let pressed = wait_completion(&registry, Duration::from_secs(5)).expect("press 未回传");
    assert_eq!(pressed.result.as_deref(), Ok(r#"{"pressed":true}"#));
    hotkey.reset();
}

#[test]
fn store_isolated_by_caller() {
    let dir = std::env::temp_dir().join(format!("lueying-store-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // 该环境变量只被 StoreProvider 读取;测试结束前恢复,避免影响其它用例。
    let previous = std::env::var_os("LUEYING_EXT_DATA_DIR");
    unsafe { std::env::set_var("LUEYING_EXT_DATA_DIR", &dir) };
    let registry = Registry::new();
    registry.set(Service::Store, Arc::new(StoreProvider));
    let call = |registry: &Registry, caller: &str, method: &str, args: &str, token: u64| {
        registry
            .call_as(caller, "store", method, args, token)
            .unwrap();
        wait_completion(registry, Duration::from_secs(5)).expect("store 未回传")
    };
    let set = call(
        &registry,
        "demo",
        "set",
        r#"{"key":"token","value":"one"}"#,
        1,
    );
    assert_eq!(set.result.as_deref(), Ok("{}"));
    let get = call(&registry, "demo", "get", r#"{"key":"token"}"#, 2);
    assert_eq!(get.result.as_deref(), Ok(r#"{"value":"one"}"#));
    let other = call(&registry, "other", "get", r#"{"key":"token"}"#, 3);
    assert_eq!(other.result.as_deref(), Ok(r#"{"value":null}"#));
    let listed = call(&registry, "demo", "list", "", 4);
    assert_eq!(listed.result.as_deref(), Ok(r#"{"keys":["token"]}"#));
    let _ = std::fs::remove_dir_all(&dir);
    match previous {
        Some(value) => unsafe { std::env::set_var("LUEYING_EXT_DATA_DIR", value) },
        None => unsafe { std::env::remove_var("LUEYING_EXT_DATA_DIR") },
    }
}

#[test]
fn deliver_units_normalize_lines_and_tabs() {
    assert!(super::deliver_units("").is_empty());
    assert_eq!(
        super::deliver_units("a\r\nb\tc\rd"),
        "a\rb cd".encode_utf16().collect::<Vec<_>>()
    );
    // 代理对按两个 UTF-16 单元送,不能按 char 截断。
    assert_eq!(
        super::deliver_units("😀"),
        "😀".encode_utf16().collect::<Vec<_>>()
    );
}

#[test]
fn notify_ok_empty_deliver_and_panel_without_ui() {
    let registry = Registry::new();
    registry.set(Service::Notify, Arc::new(NotifyProvider));
    registry.set(Service::Output, Arc::new(OutputProvider));
    registry.set(Service::Panel, Arc::new(PanelProvider::new()));
    registry
        .call("notify", "show", r#"{"title":"标题","body":"正文"}"#, 1)
        .unwrap();
    let completion = wait_completion(&registry, Duration::from_secs(5)).expect("通知未回传");
    assert_eq!(completion.result.as_deref(), Ok("{}"));

    // 空文本不注入按键;真上屏由 win-e2e 在 RichEdit 上验证。
    registry
        .call("output", "deliver", r#"{"text":""}"#, 2)
        .unwrap();
    let completion = wait_completion(&registry, Duration::from_secs(5)).expect("output 未回传");
    assert_eq!(completion.result.as_deref(), Ok(r#"{"delivered":false}"#));

    registry
        .call("panel", "show", r#"{"title":"板","items":["甲"]}"#, 3)
        .unwrap();
    let completion = wait_completion(&registry, Duration::from_secs(5)).expect("panel 未回传");
    assert_eq!(
        completion.result.as_deref().map_err(String::as_str),
        Err("宿主面板不可用:UI 未启动")
    );
}
