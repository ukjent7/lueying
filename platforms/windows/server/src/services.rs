//! 服务端持有的两个提供者:`output` 把文本注入当前焦点,`panel` 由选择面板
//! 显示后回传用户选择。扩展宿主经 stdio 请求它们(见 `lueying_ext::proto::link`)。

use std::sync::Mutex;
use std::sync::mpsc;

use lueying_ext::host::{Done, Provider};
use serde_json::json;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput,
};

fn one_method(provider: &str, method: &str, expected: &str) -> Result<(), String> {
    if method == expected {
        Ok(())
    } else {
        Err(format!("{provider} 没有方法 {method:?}"))
    }
}

/// 宿主选择面板的一次请求;窗口线程显示后经 `done` 回传 `{"selected": n|null}`。
pub struct PanelJob {
    pub panel: crate::host_panel::HostPanel,
    pub done: Done,
}

/// 宿主选择面板的投递端;界面线程装好请求通道与唤醒方式,未装时明确报错。
#[derive(Default)]
pub struct PanelProvider {
    queue: Mutex<Option<Queue>>,
}

/// 请求通道,以及投递后叫醒界面线程的办法。
struct Queue {
    sender: mpsc::SyncSender<PanelJob>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl PanelProvider {
    /// 没装界面线程的空提供者;no-UI 模式与服务单测走这条路径。
    pub fn new() -> Self {
        Self::default()
    }

    /// 装上请求通道与唤醒方式;重复装会替换旧的。
    pub fn install(
        &self,
        sender: mpsc::SyncSender<PanelJob>,
        notify: impl Fn() + Send + Sync + 'static,
    ) {
        *self.queue.lock().unwrap_or_else(|p| p.into_inner()) = Some(Queue {
            sender,
            notify: Box::new(notify),
        });
    }
}

impl Provider for PanelProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("panel", method, "show")?;
        let panel = crate::host_panel::HostPanel::parse(args)?;
        let guard = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        let Some(queue) = guard.as_ref() else {
            done.complete(Err("宿主面板不可用:UI 未启动".into()));
            return Ok(());
        };
        match queue.sender.try_send(PanelJob { panel, done }) {
            Ok(()) => (queue.notify)(),
            Err(mpsc::TrySendError::Full(job)) => {
                job.done.complete(Err("宿主面板正忙".into()));
            }
            Err(mpsc::TrySendError::Disconnected(job)) => {
                job.done
                    .complete(Err("宿主面板不可用:界面线程已退出".into()));
            }
        }
        Ok(())
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
mod tests {
    use super::*;

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
}
