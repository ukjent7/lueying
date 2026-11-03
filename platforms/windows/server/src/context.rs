//! 上下文服务:读用户当前窗口里选中的文字。
//!
//! UIA 线程常驻,COM 与 UIA 实例只初始化一次;扩展清单要在
//! `capabilities` 里申请 `selected-text` 才会放行——每一级上下文单独
//! 同意,没授权就当没有这一级。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lueying_extwasm::{Done, Provider};
use serde_json::json;

use crate::uia::SelectionReader;

/// 上下文提供者:按扩展 id 记它申请的权限,没申请 `selected-text`
/// 的一律拒绝。
pub struct ContextProvider {
    reader: Option<Arc<SelectionReader>>,
    grants: Mutex<HashMap<String, Vec<String>>>,
}

impl ContextProvider {
    pub fn new() -> Self {
        Self {
            reader: crate::uia::selection_reader().map(Arc::new),
            grants: Mutex::new(HashMap::new()),
        }
    }

    /// 灌入本次扩展重载里各扩展申请的权限。
    pub fn set_grants(&self, grants: Vec<(String, Vec<String>)>) {
        let map: HashMap<String, Vec<String>> = grants.into_iter().collect();
        *self.grants.lock().expect("上下文权限锁") = map;
    }

    /// 扩展是否拿到这一级上下文。
    fn allowed(&self, caller: &str) -> bool {
        self.grants
            .lock()
            .expect("上下文权限锁")
            .get(caller)
            .is_some_and(|capabilities| {
                capabilities
                    .iter()
                    .any(|capability| capability == "selected-text")
            })
    }
}

impl Provider for ContextProvider {
    fn call(&self, caller: &str, method: &str, _args: &str, done: Done) -> Result<(), String> {
        if method != "selection" {
            done.complete(Err(format!("context 没有方法 {method:?}")));
            return Ok(());
        }
        if !self.allowed(caller) {
            done.complete(Err("扩展没有申请 selected-text 权限".into()));
            return Ok(());
        }
        let Some(reader) = self.reader.clone() else {
            done.complete(Err("UIA 读取线程不可用".into()));
            return Ok(());
        };
        // 读一次 UIA 可能上百毫秒,放到独立线程;扩展卸载时等待侧立刻取消。
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
