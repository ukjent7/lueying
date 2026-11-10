//! Windows 服务提供者:T3 服务缝的平台实现。
//!
//! 提供者从扩展宿主线程被 [`lueying_ext::host::Registry::call`] 调进来,慢操作
//! (截图、识别)在专用线程跑完经完成队列回传,不占派发线程。`output` 用
//! Unicode 注入把文本送进当前焦点,`panel` 由独立的可选焦点窗口显示后回传
//! 用户选择。
//!
//! 截图文件落在临时目录 `lueying-capture`,文件名带毫秒时间戳;旧文件的
//! 清理随 T8 的预算与可见性一起做。热键在扩展线程上注册,WM_HOTKEY 由
//! [`HotkeyProvider::pump`] 在同一线程收取:扩展 `press` 就地兑现,用户
//! 绑定行号回带给调用方派发。

use lueying_ext::host::{Done, Provider};

pub use audio::{AsrProvider, AudioProvider};
pub use capture::CaptureProvider;
pub use clipboard::ClipboardProvider;
pub use hotkey::HotkeyProvider;
pub use network::NetworkProvider;
pub use ocr::OcrProvider;
pub use store::StoreProvider;

mod audio;
mod capture;
mod clipboard;
mod hotkey;
mod network;
mod ocr;
mod store;

#[cfg(test)]
use capture::parse_region;

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

#[cfg(all(test, windows))]
mod tests;
