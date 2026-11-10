//! 上下文服务:读用户当前窗口里选中的文字。
//!
//! UIA 线程常驻,COM 与 UIA 实例只初始化一次。扩展宿主先核对清单里的
//! `selected-text` 权限,通过了才把请求转到这里。

use std::sync::Arc;

use lueying_ext::host::{Done, Provider};
use serde_json::json;

use crate::uia::SelectionReader;

/// 上下文提供者:读用户当前窗口里选中的文字。权限核对在扩展宿主侧做完才转过来。
pub struct ContextProvider {
    reader: Option<Arc<SelectionReader>>,
}

impl ContextProvider {
    pub fn new() -> Self {
        Self {
            reader: crate::uia::selection_reader().map(Arc::new),
        }
    }
}

impl Provider for ContextProvider {
    fn call(&self, _caller: &str, method: &str, _args: &str, done: Done) -> Result<(), String> {
        if method != "selection" {
            done.complete(Err(format!("context 没有方法 {method:?}")));
            return Ok(());
        }
        let Some(reader) = self.reader.clone() else {
            done.complete(Err("UIA 读取线程不可用".into()));
            return Ok(());
        };
        // 读一次 UIA 可能上百毫秒,放到独立线程;宿主侧的调用有自己的时限。
        std::thread::Builder::new()
            .name("ext-context".into())
            .spawn(move || {
                let reply = match reader.read() {
                    Some(text) => Ok(json!({ "text": text }).to_string()),
                    None => Err("读不到选中文字(没有焦点元素或被系统拒绝)".to_string()),
                };
                done.complete(reply);
            })
            .map_err(|error| format!("上下文线程启动失败: {error}"))?;
        Ok(())
    }
}
