//! 8M 按键重排:按键只取一份快照,推理与候选提升都在后台线程完成。
//!
//! 结果落地前核对 revision/页/高亮:输入已经往前走了就丢弃。按键路径因此
//! 只花引擎本身的时间,模型快慢不再决定一次按键的延迟。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use lueying_rerank::settle::result_is_current;
use lueying_rerank::{LatencyMetrics, Model, Rerank};

use crate::imp::{Session, State};

/// 一次重排的快照;不持有会话或推理缓存。
pub(super) struct Job {
    pub(super) session: u64,
    /// 发起时的焦点/上下文代次;中途换了输入范围就丢弃。
    pub(super) seq: u64,
    /// 重排输入快照;上下文、候选与版本号都在里面。
    pub(super) snap: lueying_core::session::Snapshot,
}

impl Job {
    pub(super) fn matches(&self, s: &Session) -> bool {
        !s.expanded
            && s.context_seq == self.seq
            && result_is_current(
                s.engine.revision(),
                self.snap.revision,
                s.engine.page(),
                s.engine.highlight(),
                s.engine.composition().is_some(),
            )
    }

    pub(super) fn is_current(&self, state: &State) -> bool {
        state
            .session(self.session)
            .is_some_and(|s| self.matches(&s.lock()))
    }
}

/// 每个推理线程最多保留一个会话的历史和一份工作区,不是每个宿主线程各留一份。
pub(super) struct Worker<'a> {
    model: Model,
    metrics: &'a Arc<LatencyMetrics>,
    cached: Option<(u64, Rerank)>,
    last_used: Instant,
}

impl<'a> Worker<'a> {
    pub(super) fn new(model: &Model, metrics: &'a Arc<LatencyMetrics>) -> Self {
        Self {
            model: model.clone(),
            metrics,
            cached: None,
            last_used: Instant::now(),
        }
    }

    pub(super) fn rerank(&mut self, session: u64) -> &mut Rerank {
        let (owner, rerank) = self.cached.get_or_insert_with(|| {
            (
                session,
                Rerank::with_metrics(&self.model, Arc::clone(self.metrics)),
            )
        });
        if *owner != session {
            rerank.clear_history();
            *owner = session;
        }
        self.last_used = Instant::now();
        rerank
    }

    pub(super) fn idle_wait(&mut self) -> Option<Duration> {
        self.cached.as_ref()?;
        if let Some(wait) = Duration::from_secs(5).checked_sub(self.last_used.elapsed()) {
            return Some(wait);
        }
        self.cached = None;
        // 工作区在本线程分配和释放;空闲时再归还空闲页,不碰按键线程。
        unsafe { libmimalloc_sys::mi_collect(true) };
        None
    }
}

/// 按键或上下文变化后调用:门控不通过就什么也不做,永不阻塞调用方。
pub(super) fn schedule(state: &State, session: u64, s: &Session) {
    let Some(tx) = state.sync_tx.as_ref() else {
        return;
    };
    // 快捷候选固定在本轮插入的位置,8M 模型不能把它们换走;置顶词同理。
    if s.engine.context().is_empty() || lueying_core::session::pinned_candidates(&s.engine) {
        return;
    }
    let Some(snap) = lueying_core::session::snapshot(&s.engine) else {
        return;
    };
    // 通道满说明上一键的结果还没出来:直接丢掉这一次,排队只会让结果更旧。
    let _ = tx.try_send(Job {
        session,
        seq: s.context_seq,
        snap,
    });
}

pub(super) fn spawn(state: Arc<State>, rx: mpsc::Receiver<Job>) {
    std::thread::Builder::new()
        .name("lueying-sync-rerank".into())
        .spawn(move || {
            let Some(model) = &state.sync_model else {
                return;
            };
            let mut worker = Worker::new(model, &state.rerank_sync_metrics);
            loop {
                let job = match worker.idle_wait() {
                    Some(wait) => rx.recv_timeout(wait),
                    None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                };
                match job {
                    Ok(job) => {
                        // 一次推理 panic 不能让整条线程退出;丢掉本次结果即可。
                        let _ = catch_unwind(AssertUnwindSafe(|| run(&state, &job, &mut worker)));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .expect("启动按键重排线程失败");
}

fn run(state: &State, job: &Job, worker: &mut Worker<'_>) {
    if !job.is_current(state) {
        return;
    }
    let ranking =
        lueying_core::session::rank_context_with_gap(worker.rerank(job.session), &job.snap);
    if ranking.order.is_empty() {
        return;
    }
    let skip_25m =
        state.settled_model.is_some() && lueying_core::session::can_skip_25m(&job.snap, &ranking);
    let Some(handle) = state.session(job.session) else {
        return;
    };
    let mut s = handle.lock();
    if !job.matches(&s) {
        return;
    }
    let changed = s.engine.reorder_candidates(&ranking.order);
    s.skip_25m_at = skip_25m.then_some((s.engine.revision(), s.context_seq));
    if changed {
        crate::settle::refresh_after_sync(state, job.session, &s);
        crate::imp::refresh_view(&mut s);
        crate::imp::sync_pending(&mut s);
        if handle.publish(&s) {
            crate::imp::touch_view(state);
        }
    }
}
