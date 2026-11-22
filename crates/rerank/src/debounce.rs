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

    /// Update the payload without counting a background update as a new keystroke.
    pub fn refresh(&mut self, session: u64, now_ms: u64, job: T) {
        let at = self.pending.get(&session).map_or(now_ms, |(at, _)| *at);
        self.submit(session, at, job);
    }

    pub fn cancel(&mut self, session: u64) {
        self.pending.remove(&session);
    }

    /// 只取一个到期任务;推理结束后重新检查队列,让其间的替换和取消生效。
    pub fn pop_due(&mut self, now_ms: u64) -> Option<T> {
        let session = self
            .pending
            .iter()
            .filter(|(_, (at, _))| now_ms.saturating_sub(*at) >= self.delay_ms)
            .map(|(&session, (at, _))| (*at, session))
            .min()?
            .1;
        self.pending.remove(&session).map(|(_, job)| job)
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

#[cfg(test)]
mod tests {
    use super::Debounce;

    #[test]
    fn replacement_and_cancel_still_apply_while_a_due_job_is_running() {
        let mut queue = Debounce::new(120);
        queue.submit(1, 0, "first");
        queue.submit(2, 0, "cancelled");
        queue.submit(3, 0, "stale");
        assert_eq!(queue.pop_due(120), Some("first"));

        queue.cancel(2);
        for now in 121..=150 {
            queue.submit(3, now, "latest");
        }
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(queue.pop_due(269), None);
        assert_eq!(queue.pop_due(270), Some("latest"));
        assert_eq!(queue.pop_due(270), None);
    }

    #[test]
    fn background_reorder_refreshes_the_payload_but_not_the_deadline() {
        let mut queue = Debounce::new(120);
        queue.submit(1, 10, "before 8M");
        queue.refresh(1, 90, "after 8M");
        assert_eq!(queue.pop_due(129), None);
        assert_eq!(queue.pop_due(130), Some("after 8M"));
    }
}
