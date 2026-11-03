//! 25M 停顿重排:按键后只记一个每会话待办,静默 120ms 才跑模型;
//! 推理不持有 `sessions` 锁,回来时用 revision/页/高亮确认结果仍然有效。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lueying_rerank::debounce::Debounce;
use lueying_rerank::settle::result_is_current;

use crate::imp::{Session, State, refresh_view};

const DELAY_MS: u64 = 120;

pub(super) enum SettleMessage {
    Job(SettleJob),
    Cancel { session: u64 },
}

pub(super) struct SettleJob {
    session: u64,
    /// 发起时的焦点/上下文代次;中途换了输入范围就丢弃。
    seq: u64,
    /// 重排输入快照;上下文、候选与版本号都在里面。
    snap: lueying_session::Snapshot,
    rerank: Arc<Mutex<lueying_rerank::Rerank>>,
}

/// 按键后调用:有组合就排队等停顿,没有就撤掉旧任务。
pub(super) fn schedule(state: &State, session: u64, s: &Session) {
    let Some(tx) = state.settle_tx.as_ref() else {
        return;
    };
    let Some(rerank) = s.settled_rerank.as_ref() else {
        return;
    };
    if s.engine.page() != 0 || s.engine.highlight() != 0 {
        let _ = tx.send(SettleMessage::Cancel { session });
        return;
    }
    // 快捷候选必须留在插入位置;停顿重排不能把它们换走,
    // 否则空格上屏的会是另一个词。置顶词同理。
    if lueying_session::pinned_candidates(&s.engine) {
        let _ = tx.send(SettleMessage::Cancel { session });
        return;
    }
    let Some(snap) = lueying_session::snapshot(&s.engine) else {
        let _ = tx.send(SettleMessage::Cancel { session });
        return;
    };
    let _ = tx.send(SettleMessage::Job(SettleJob {
        session,
        seq: s.context_seq,
        snap,
        rerank: Arc::clone(rerank),
    }));
}

pub(super) fn cancel(state: &State, session: u64) {
    if let Some(tx) = state.settle_tx.as_ref() {
        let _ = tx.send(SettleMessage::Cancel { session });
    }
}

pub(super) fn spawn(state: Arc<State>, rx: mpsc::Receiver<SettleMessage>) {
    std::thread::Builder::new()
        .name("lueying-settle".into())
        .spawn(move || {
            let start = Instant::now();
            let mut debounce: Debounce<SettleJob> = Debounce::new(DELAY_MS);
            loop {
                let now = elapsed_ms(start);
                let wait = debounce
                    .next_due_ms()
                    .map_or(1000, |due| due.saturating_sub(now))
                    .clamp(1, 1000);
                match rx.recv_timeout(Duration::from_millis(wait)) {
                    Ok(SettleMessage::Job(job)) => {
                        let now = elapsed_ms(start);
                        debounce.submit(job.session, now, job);
                    }
                    Ok(SettleMessage::Cancel { session }) => debounce.cancel(session),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
                let now = elapsed_ms(start);
                for job in debounce.take_due(now) {
                    // 一次推理 panic 不能让整条 25M 线程退出;丢掉本次结果即可。
                    let _ = catch_unwind(AssertUnwindSafe(|| {
                        let promote = lueying_session::decide_settled(&job.rerank, &job.snap);
                        apply(&state, &job, promote);
                    }));
                }
            }
        })
        .expect("启动停顿重排线程失败");
}

fn apply(state: &State, job: &SettleJob, promote: Option<usize>) {
    let Some(index) = promote else {
        return;
    };
    let Some(handle) = state.session(job.session) else {
        return;
    };
    let mut s = handle.lock();
    if s.context_seq != job.seq {
        return;
    }
    if !result_is_current(
        s.engine.revision(),
        job.snap.revision,
        s.engine.page(),
        s.engine.highlight(),
        s.engine.composition().is_some(),
    ) {
        return;
    }
    if s.engine.promote(index) {
        refresh_view(&mut s);
        crate::imp::sync_pending(&mut s);
        if handle.publish(&s) {
            crate::imp::touch_view(state);
        }
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}
