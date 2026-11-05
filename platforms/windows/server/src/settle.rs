//! 25M 停顿重排:按键后只记一个每会话待办,静默 120ms 才跑模型;
//! 推理不持有 `sessions` 锁,回来时用 revision/页/高亮确认结果仍然有效。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lueying_rerank::debounce::Debounce;
use lueying_rerank::settle::result_is_current;

use crate::imp::{Session, State, refresh_view, sync_pending};

const DELAY_MS: u64 = 120;

/// 停顿重排算出的首选,等 TSF 下一次 `Sync` 与内联文字一起生效。
#[derive(Clone, Copy)]
pub(super) struct SettlePick {
    index: usize,
    revision: u64,
}

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
    // 开关关掉时不排任何停顿任务,首选只随按键变化。
    if !state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .settled_promote
    {
        return;
    }
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
    // 只记下待生效的首选,不改引擎也不刷新候选窗。内联文字要等 TSF 的下一次 Sync
    // 才写回文档;候选窗若先变,空格上屏的就是用户没看到的那个。
    s.settle_pick = Some(SettlePick {
        index,
        revision: job.snap.revision,
    });
}

/// `Sync` 时调用:停顿算出的首选仍然有效,就在这一刻与内联文字一起切过去。
pub(super) fn take_pick(s: &mut Session) {
    let Some(pick) = s.settle_pick.take() else {
        return;
    };
    if !result_is_current(
        s.engine.revision(),
        pick.revision,
        s.engine.page(),
        s.engine.highlight(),
        s.engine.composition().is_some(),
    ) {
        return;
    }
    if s.engine.promote(pick.index) {
        refresh_view(s);
        sync_pending(s);
        s.view.promotions = s.view.promotions.wrapping_add(1);
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}
