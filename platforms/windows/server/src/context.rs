//! 上下文服务:读用户当前窗口里选中的文字。
//!
//! 读取在 UIA 线程上做,读完直接经 `Done` 回传;扩展宿主先核对清单里的
//! `selected-text` 权限,通过了才把请求转到这里。

use std::sync::mpsc::{SyncSender, TrySendError};

use lueying_ext::host::{Done, Provider};

use crate::uia::Work;

/// 上下文提供者:读用户当前窗口里选中的文字。权限核对在扩展宿主侧做完才转过来。
pub struct ContextProvider {
    uia: SyncSender<Work>,
}

impl ContextProvider {
    pub fn new(uia: SyncSender<Work>) -> Self {
        Self { uia }
    }
}

impl Provider for ContextProvider {
    fn call(&self, _caller: &str, method: &str, _args: &str, done: Done) -> Result<(), String> {
        if method != "selection" {
            done.complete(Err(format!("context 没有方法 {method:?}")));
            return Ok(());
        }
        // 队列满时直接回忙,不在这里排队等。
        match self.uia.try_send(Work::Selection(done)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(work)) => {
                reject(work, "UIA 读取线程忙");
                Ok(())
            }
            Err(TrySendError::Disconnected(work)) => {
                reject(work, "UIA 读取线程不可用");
                Ok(())
            }
        }
    }
}

fn reject(work: Work, message: &str) {
    if let Work::Selection(done) = work {
        done.complete(Err(message.to_string()));
    }
}
