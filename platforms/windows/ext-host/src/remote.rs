//! 需要服务端完成的服务(输出、选择面板、上下文):请求经行协议发出,应答由读线程按 id 回填。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lueying_ext::host::{Done, Provider};
use lueying_ext::proto::link::ToServer;

use crate::channel::Outbound;

/// 等待应答的调用:请求 id → 完成句柄。
pub type Pending = Arc<Mutex<HashMap<u64, Done>>>;

/// 转给服务端的服务提供者。
pub struct Remote {
    service: &'static str,
    out: Outbound,
    pending: Pending,
    next: Arc<AtomicU64>,
}

impl Remote {
    pub fn new(
        service: &'static str,
        out: Outbound,
        pending: Pending,
        next: Arc<AtomicU64>,
    ) -> Self {
        Self {
            service,
            out,
            pending,
            next,
        }
    }
}

impl Provider for Remote {
    fn call(&self, caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id, done);
        self.out.send(&ToServer::Request {
            id,
            caller: caller.to_string(),
            service: self.service.to_string(),
            method: method.to_string(),
            args: args.to_string(),
        });
        Ok(())
    }
}
