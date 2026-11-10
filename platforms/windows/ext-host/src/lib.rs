//! 掠影扩展宿主:运行全部扩展(Wasm 组件与原生进程),允许联网。
//!
//! 输入法本体(服务端)不加载任何扩展、不联网;它拉起本进程并经标准输入输出
//! 收发行协议(见 `lueying_ext::proto::link`)。本进程崩溃或卡死时服务端照常打字,
//! 外观补丁与服务结果都带超时,过期的一律丢弃。

mod candidates;
mod channel;
mod remote;
mod selftest;
#[cfg(windows)]
mod services;
mod state;
mod worker;

use std::sync::Arc;
use std::sync::mpsc;

pub use selftest::selftest;
pub use state::Paths;

/// 进程主入口:装本机目录,起标准输入读线程,然后跑主循环直到服务端关闭。
pub fn run(paths: Paths) -> i32 {
    state::install(paths);
    let out = channel::Outbound::new();
    let pending: remote::Pending = Arc::default();
    let (inbox_tx, inbox_rx) = mpsc::channel();
    {
        let pending = Arc::clone(&pending);
        let spawned = std::thread::Builder::new()
            .name("ext-host-read".into())
            .spawn(move || {
                channel::read_lines(|message| match message {
                    // 应答直接回填给等待的调用;其余消息交给主循环。
                    lueying_ext::proto::link::ToHost::Reply { id, result } => {
                        let done = pending
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .remove(&id);
                        if let Some(done) = done {
                            done.complete(result);
                        }
                    }
                    other => {
                        let _ = inbox_tx.send(other);
                    }
                });
                // 服务端关了输入即退出:扩展可能正卡在死循环里,主循环不一定能自己结束。
                std::process::exit(0);
            });
        if let Err(error) = spawned {
            eprintln!("扩展宿主读线程启动失败: {error}");
            return 1;
        }
    }
    worker::run(inbox_rx, out, pending)
}
