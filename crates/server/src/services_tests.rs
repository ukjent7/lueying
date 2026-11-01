//! 服务提供者的 Windows 集成验证:真 GDI 截图、真 WinRT 识别、热键语义。
//!
//! CI runner 可能缺 OCR 语言包:识别只接受确定性的「没有可用引擎」失败,
//! 其余错误都算红。截图与识别走真实平台路径,这部分是 T3 每服务
//! Windows E2E 的落点;文件在验证后删掉。

use std::sync::Arc;
use std::time::Duration;

use lueying_extwasm::{Registry, Service};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
    VK_SPACE,
};

use crate::services::{
    CaptureProvider, HotkeyProvider, NotifyProvider, OcrProvider, OutputProvider, PanelProvider,
    StoreProvider,
};

const WAIT: Duration = Duration::from_secs(30);

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
    let result = registry.call("capture", "shot", "", WAIT).expect("截图失败");
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
    let shot = registry.call("capture", "shot", "", WAIT).expect("截图失败");
    let file: String = {
        let parsed: serde_json::Value = serde_json::from_str(&shot).unwrap();
        parsed["file"].as_str().unwrap().to_string()
    };
    let args = serde_json::json!({ "file": file }).to_string();
    match registry.call("ocr", "recognize", &args, Duration::from_secs(120)) {
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
        registry.call(
            "hotkey",
            "register",
            &format!(r#"{{"id":"{id}","modifiers":["ctrl","alt"],"key":"K"}}"#),
            Duration::from_secs(5),
        )
    };
    let first = register(&registry, "paste");
    assert!(first.is_ok(), "{first:?}");
    let second = register(&registry, "paste");
    assert_eq!(second, Err("热键已注册: paste".into()));
    registry
        .call(
            "hotkey",
            "unregister",
            r#"{"id":"paste"}"#,
            Duration::from_secs(5),
        )
        .expect("注销失败");
    let third = register(&registry, "paste");
    assert!(third.is_ok(), "{third:?}");
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
    let registry = Arc::new(Registry::new());
    let hotkey = Arc::new(HotkeyProvider::default());
    let provider: Arc<HotkeyProvider> = Arc::clone(&hotkey);
    registry.set(Service::Hotkey, provider);
    registry
        .call(
            "hotkey",
            "register",
            r#"{"id":"paste","modifiers":["ctrl","alt"],"key":"N"}"#,
            Duration::from_secs(5),
        )
        .expect("注册失败");
    let missing = registry.call(
        "hotkey",
        "press",
        r#"{"id":"ghost"}"#,
        Duration::from_secs(5),
    );
    assert_eq!(missing, Err("热键未注册: ghost".into()));
    // press 阻塞等待触发;热键注册在主线程,WM_HOTKEY 也只能在主线程 pump。
    let waiting = {
        let registry = Arc::clone(&registry);
        std::thread::spawn(move || {
            registry.call(
                "hotkey",
                "press",
                r#"{"id":"paste"}"#,
                Duration::from_secs(5),
            )
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    send_combo(VIRTUAL_KEY(0x4E)); // N
    std::thread::sleep(Duration::from_millis(300));
    hotkey.pump();
    let pressed = waiting.join().expect("等待线程");
    assert_eq!(pressed, Ok(r#"{"pressed":true}"#.into()));
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
    let call = |caller: &str, method: &str, args: &str| {
        registry.call(caller, "store", method, args, Duration::from_secs(5))
    };
    assert_eq!(
        call("demo", "set", r#"{"key":"token","value":"one"}"#),
        Ok("{}".into())
    );
    assert_eq!(
        call("demo", "get", r#"{"key":"token"}"#),
        Ok(r#"{"value":"one"}"#.into())
    );
    assert_eq!(
        call("other", "get", r#"{"key":"token"}"#),
        Ok(r#"{"value":null}"#.into())
    );
    assert_eq!(
        call("demo", "list", ""),
        Ok(r#"{"keys":["token"]}"#.into())
    );
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
    assert_eq!(
        registry.call(
            "notify",
            "show",
            r#"{"title":"标题","body":"正文"}"#,
            Duration::from_secs(5)
        ),
        Ok("{}".into())
    );
    // 空文本不注入按键;真上屏由 win-e2e 在 RichEdit 上验证。
    assert_eq!(
        registry.call(
            "output",
            "deliver",
            r#"{"text":""}"#,
            Duration::from_secs(5)
        ),
        Ok(r#"{"delivered":false}"#.into())
    );
    assert_eq!(
        registry.call(
            "panel",
            "show",
            r#"{"title":"板","items":["甲"]}"#,
            Duration::from_secs(5)
        ),
        Err("宿主面板不可用:UI 未启动".into())
    );
}
