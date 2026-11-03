//! Windows 服务提供者:T3 服务缝的平台实现。
//!
//! 提供者从扩展宿主线程被 [`lueying_extwasm::Registry::call`] 调进来,慢操作
//! (截图、识别)在专用线程跑完经完成队列回传,不占派发预算。`output` 用
//! Unicode 注入把文本送进当前焦点,`panel` 由独立的可选焦点窗口显示后回传
//! 用户选择。
//!
//! 截图文件落在临时目录 `lueying-capture`,文件名带毫秒时间戳;旧文件的
//! 清理随 T8 的预算与可见性一起做。热键在扩展线程上注册,WM_HOTKEY 由
//! [`HotkeyProvider::pump`] 在同一线程收取:扩展 `press` 就地兑现,用户
//! 绑定行号回带给调用方派发。

use lueying_extwasm::{Done, Provider};
use serde_json::json;
use std::sync::Mutex;
use std::sync::mpsc;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput,
};

#[path = "services_audio.rs"]
mod audio;
#[path = "services_capture.rs"]
mod capture;
#[path = "services_clipboard.rs"]
mod clipboard;
#[path = "services_hotkey.rs"]
mod hotkey;
#[path = "services_network.rs"]
mod network;
#[path = "services_ocr.rs"]
mod ocr;
#[path = "services_store.rs"]
mod store;

pub use audio::{AsrProvider, AudioProvider};
pub use capture::CaptureProvider;
#[cfg(test)]
use capture::parse_region;
pub use clipboard::ClipboardProvider;
pub use hotkey::HotkeyProvider;
pub use network::{NetworkProvider, network_broker};
pub use ocr::OcrProvider;
pub use store::StoreProvider;

/// 单方法服务的入口守卫:方法名不符时回 provider 侧错误,与 [`Provider`] 约定一致。
fn one_method(provider: &str, method: &str, expected: &str) -> Result<(), String> {
    if method == expected {
        Ok(())
    } else {
        Err(format!("{provider} 没有方法 {method:?}"))
    }
}

/// 系统通知;v1 只落日志,toast 需要包标识或快捷方式激活,挂后续可见性任务。
pub struct NotifyProvider;

impl Provider for NotifyProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("notify", method, "show")?;
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let title = parsed["title"].as_str().unwrap_or_default();
        let body = parsed["body"].as_str().unwrap_or_default();
        println!("扩展通知:{title}:{body}");
        done.complete(Ok("{}".into()));
        Ok(())
    }
}

/// 宿主选择面板的一次请求;窗口线程显示后经 `done` 回传 `{"selected": n|null}`。
pub struct PanelJob {
    pub panel: crate::host_panel::HostPanel,
    pub done: Done,
}

/// 宿主选择面板;窗口线程由 `ui_panel_host::spawn` 装填,未启动时明确报错。
#[derive(Default)]
pub struct PanelProvider {
    sender: Mutex<Option<mpsc::SyncSender<PanelJob>>>,
}

impl PanelProvider {
    /// 没装窗口线程的空提供者;no-UI 模式与服务单测走这条路径。
    pub fn new() -> Self {
        Self::default()
    }

    /// 装窗口线程的请求通道;重复装会替换旧通道。
    pub fn install(&self, sender: mpsc::SyncSender<PanelJob>) {
        *self.sender.lock().unwrap_or_else(|p| p.into_inner()) = Some(sender);
    }
}

impl Provider for PanelProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("panel", method, "show")?;
        let panel = crate::host_panel::HostPanel::parse(args)?;
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let Some(sender) = sender else {
            done.complete(Err("宿主面板不可用:UI 未启动".into()));
            return Ok(());
        };
        match sender.try_send(PanelJob { panel, done }) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(job)) => {
                job.done.complete(Err("宿主面板正忙".into()));
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(job)) => {
                job.done
                    .complete(Err("宿主面板不可用:窗口线程已退出".into()));
                Ok(())
            }
        }
    }
}

/// 上屏:把文本按 Unicode 注入当前焦点。`delivered:false` 只表示没有可送的字。
pub struct OutputProvider;

impl Provider for OutputProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("output", method, "deliver")?;
        let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
        let text = parsed["text"]
            .as_str()
            .ok_or_else(|| "缺 text".to_string())?;
        match deliver_text(text) {
            Ok(delivered) => {
                done.complete(Ok(json!({ "delivered": delivered }).to_string()));
                Ok(())
            }
            Err(error) => {
                done.complete(Err(error));
                Ok(())
            }
        }
    }
}

/// 把文本按 Unicode 注入当前焦点。走 `SendInput` 的 VK_PACKET 通道,不经过
/// 键盘布局与输入法组句;Electron 与自绘控件收 WM_CHAR,不依赖 TSF 会话。
/// 换行按回车送(编辑控件里就是分段),制表符换成空格,避免焦点在对话框里跳走。
/// 对外 commit 通道(`Request::ExternalCommit`)复用同一条上屏路径。
pub fn deliver_text(text: &str) -> Result<bool, String> {
    let units = deliver_units(text);
    if units.is_empty() {
        return Ok(false);
    }
    // 一次送太多事件会拖住调用线程;256 字一批,失败立即停。
    for chunk in units.chunks(256) {
        let mut inputs = Vec::with_capacity(chunk.len() * 2);
        for &unit in chunk {
            inputs.push(unicode_input(unit, false));
            inputs.push(unicode_input(unit, true));
        }
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) } as usize;
        if sent != inputs.len() {
            let error = std::io::Error::last_os_error();
            return Err(format!("上屏被系统拦截({sent}/{}):{error}", inputs.len()));
        }
    }
    Ok(true)
}

/// 文本 → 要注入的 UTF-16 单元:换行按回车、制表符换空格,孤立 CR 丢弃
/// (CRLF 先归一,避免一个换行变成两个空行);代理对保持两个单元。
fn deliver_units(text: &str) -> Vec<u16> {
    let normalized = text.replace("\r\n", "\n");
    let mut units = Vec::with_capacity(normalized.len());
    for ch in normalized.chars() {
        match ch {
            '\n' => units.push('\r' as u16),
            '\t' => units.push(' ' as u16),
            '\r' => {}
            _ => {
                let mut buf = [0u16; 2];
                units.extend_from_slice(ch.encode_utf16(&mut buf));
            }
        }
    }
    units
}

/// 一个 Unicode 键事件;`up` 决定按下还是抬起。
fn unicode_input(unit: u16, up: bool) -> INPUT {
    let mut input = INPUT::default();
    input.r#type = INPUT_KEYBOARD;
    input.Anonymous.ki.wScan = unit;
    input.Anonymous.ki.dwFlags = if up {
        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
    } else {
        KEYEVENTF_UNICODE
    };
    input
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod services_tests;
