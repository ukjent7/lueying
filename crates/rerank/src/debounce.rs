//! 停顿重排的去抖队列:每个会话只保留最后一次提交,静默 `delay_ms` 后才到期。
//!
//! 时间由调用方以毫秒传入,队列本身不读时钟,便于在 E2E 里确定性地验证
//! 「连续按键只算最后一次」「多个会话互不拖累」「取消后不再执行」。

use std::collections::HashMap;

pub struct Debounce<T> {
    delay_ms: u64,
    pending: HashMap<u64, (u64, T)>,
}

impl<T> Debounce<T> {
    pub fn new(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            pending: HashMap::new(),
        }
    }

    /// 提交/替换一个会话的待办。到达时间取最后一次提交。
    pub fn submit(&mut self, session: u64, now_ms: u64, job: T) {
        self.pending.insert(session, (now_ms, job));
    }

    pub fn cancel(&mut self, session: u64) {
        self.pending.remove(&session);
    }

    /// 取走所有已静默满 `delay_ms` 的待办,按会话号升序,保证执行顺序稳定。
    pub fn take_due(&mut self, now_ms: u64) -> Vec<T> {
        let mut due: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, (at, _))| now_ms.saturating_sub(*at) >= self.delay_ms)
            .map(|(&session, _)| session)
            .collect();
        due.sort_unstable();
        due.into_iter()
            .filter_map(|session| self.pending.remove(&session).map(|(_, job)| job))
            .collect()
    }

    /// 最早到期的绝对毫秒;没有待办返回 `None`。
    pub fn next_due_ms(&self) -> Option<u64> {
        self.pending
            .values()
            .map(|(at, _)| at.saturating_add(self.delay_ms))
            .min()
    }
}
