//! 25M 停顿重排:按键后只记一个每会话待办,静默 120ms 才跑模型;
//! 推理不持有 `sessions` 锁,回来时用 revision/页/高亮确认结果仍然有效。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use lueying_rerank::Model;
use lueying_rerank::debounce::Debounce;
use lueying_rerank::settle::result_is_current;

use crate::imp::{Session, State, assets, refresh_view, sync_pending};
use crate::sync_rerank::{Job, Worker};

const DELAY_MS: u64 = 120;
const MODEL_25M: &str = "rerank-25m";

pub(super) enum ModelSource {
    Preloaded(Model),
    OnDemand(Duration),
}

impl ModelSource {
    pub(super) fn open(idle_secs: u64) -> Option<Self> {
        if idle_secs == 0 {
            assets::load_model(MODEL_25M).map(Self::Preloaded)
        } else {
            assets::model_exists(MODEL_25M).then(|| Self::OnDemand(Duration::from_secs(idle_secs)))
        }
    }
}

/// 停顿重排算出的顺序,等 TSF 下一次 `Sync` 与内联文字一起生效。
pub(super) struct SettlePick {
    decision: Decision,
    revision: u64,
    seq: u64,
}

enum Decision {
    Head(usize),
    Order(Vec<usize>),
}

pub(super) struct Queue {
    started: Instant,
    pending: Mutex<Debounce<Job>>,
    changed: Condvar,
}

impl Queue {
    pub(super) fn new() -> Self {
        Self {
            started: Instant::now(),
            pending: Mutex::new(Debounce::new(DELAY_MS)),
            changed: Condvar::new(),
        }
    }

    fn submit(&self, job: Job, after_sync: bool) {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let now = elapsed_ms(self.started);
        if after_sync {
            pending.refresh(job.session, now, job);
        } else {
            pending.submit(job.session, now, job);
        }
        self.changed.notify_one();
    }

    fn cancel(&self, session: u64) {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.cancel(session);
        self.changed.notify_one();
    }

    fn next(&self, mut wait_for_idle: impl FnMut() -> Option<Duration>) -> Job {
        loop {
            let idle_wait = wait_for_idle();
            let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            let now = elapsed_ms(self.started);
            if let Some(job) = pending.pop_due(now) {
                return job;
            }
            let due_wait = pending
                .next_due_ms()
                .map(|due| Duration::from_millis(due.saturating_sub(now)));
            match idle_wait.into_iter().chain(due_wait).min() {
                Some(wait) => {
                    drop(self.changed.wait_timeout(pending, wait));
                }
                None => drop(self.changed.wait(pending)),
            }
        }
    }
}

/// 按键后调用:有组合就排队等停顿,没有就撤掉旧任务。
pub(super) fn schedule(state: &State, session: u64, s: &Session) {
    schedule_with(state, session, s, false);
}

/// 8M 改了候选序号,25M 必须拿新快照;按键计时不因后台重排而重置。
pub(super) fn refresh_after_sync(state: &State, session: u64, s: &Session) {
    if state.settled_model.is_some() {
        schedule_with(state, session, s, true);
    }
}

fn schedule_with(state: &State, session: u64, s: &Session, after_sync: bool) {
    let Some(queue) = state.settle_queue.as_ref() else {
        return;
    };
    // 开关关掉时不排任何停顿任务,首选只随按键变化。
    if !state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .settled_promote
    {
        queue.cancel(session);
        return;
    }
    if s.expanded || s.engine.page() != 0 || s.engine.highlight() != 0 {
        queue.cancel(session);
        return;
    }
    // 快捷候选必须留在插入位置;停顿重排不能把它们换走,
    // 否则空格上屏的会是另一个词。置顶词同理。
    if lueying_core::session::pinned_candidates(&s.engine) {
        queue.cancel(session);
        return;
    }
    let Some(snap) = lueying_core::session::snapshot(&s.engine) else {
        queue.cancel(session);
        return;
    };
    queue.submit(
        Job {
            session,
            seq: s.context_seq,
            snap,
        },
        after_sync,
    );
}

pub(super) fn cancel(state: &State, session: u64) {
    if let Some(queue) = state.settle_queue.as_ref() {
        queue.cancel(session);
    }
}

pub(super) fn spawn(state: Arc<State>) {
    std::thread::Builder::new()
        .name("lueying-settle".into())
        .spawn(move || {
            let Some(queue) = &state.settle_queue else {
                return;
            };
            let (mut worker, keep_alive) = match &state.settled_model {
                Some(ModelSource::Preloaded(model)) => {
                    (Some(Worker::new(model, &state.rerank_settle_metrics)), None)
                }
                Some(ModelSource::OnDemand(ttl)) => (None, Some(*ttl)),
                None => (None, None),
            };
            let mut fallback = state
                .sync_model
                .as_ref()
                .map(|model| Worker::new(model, &state.rerank_settle_metrics));
            let mut expires = None::<Instant>;
            let mut retry_at = None::<Instant>;
            loop {
                let job = queue.next(|| {
                    let model_wait = expires.map(|at| at.saturating_duration_since(Instant::now()));
                    if model_wait == Some(Duration::ZERO) {
                        worker = None;
                        expires = None;
                        state.settled_resident.store(false, Ordering::Relaxed);
                        // Both the workspace and weights were allocated on this worker.
                        unsafe { libmimalloc_sys::mi_collect(true) };
                        println!("25M idle unload");
                    }
                    model_wait
                        .filter(|wait| !wait.is_zero())
                        .into_iter()
                        .chain(worker.as_mut().and_then(Worker::idle_wait))
                        .chain(fallback.as_mut().and_then(Worker::idle_wait))
                        .min()
                });
                // 一次推理 panic 不能让整条 25M 线程退出;丢掉本次结果即可。
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if !should_run(&state, &job) {
                        return;
                    }
                    if let Some(ttl) = keep_alive
                        && worker.is_none()
                        && retry_at.is_none_or(|at| Instant::now() >= at)
                    {
                        let started = Instant::now();
                        if let Some(model) = assets::load_model(MODEL_25M) {
                            worker = Some(Worker::new(&model, &state.rerank_settle_metrics));
                            expires = Some(Instant::now() + ttl);
                            state.settled_resident.store(true, Ordering::Relaxed);
                            println!("25M cold load: {} ms", started.elapsed().as_millis());
                            // The user may have typed again while the weights were being loaded.
                            if !should_run(&state, &job) {
                                return;
                            }
                        } else {
                            retry_at = Some(Instant::now() + Duration::from_secs(30));
                        }
                    }
                    let using_25m = worker.is_some();
                    let Some(rerank) = worker.as_mut().or(fallback.as_mut()) else {
                        return;
                    };
                    let rerank = rerank.rerank(job.session);
                    let decision = if using_25m && !job.snap.context.is_empty() {
                        let order = lueying_core::session::rank_context(rerank, &job.snap);
                        (!order.is_sorted()).then_some(Decision::Order(order))
                    } else {
                        lueying_core::session::decide_settled(rerank, &job.snap).map(Decision::Head)
                    };
                    if using_25m && let Some(ttl) = keep_alive {
                        expires = Some(Instant::now() + ttl);
                    }
                    apply(&state, &job, decision);
                }));
            }
        })
        .expect("启动停顿重排线程失败");
}

fn should_run(state: &State, job: &Job) -> bool {
    let selective = {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        if !cfg.settled_promote {
            return false;
        }
        cfg.selective_25m && state.settled_model.is_some()
    };
    state.session(job.session).is_some_and(|handle| {
        let s = handle.lock();
        job.matches(&s) && !(selective && s.skip_25m_at == Some((job.snap.revision, job.seq)))
    })
}

fn apply(state: &State, job: &Job, decision: Option<Decision>) {
    let Some(decision) = decision else {
        return;
    };
    let Some(handle) = state.session(job.session) else {
        return;
    };
    let mut s = handle.lock();
    if !job.matches(&s) {
        return;
    }
    // 只记下待生效的顺序,不改引擎也不刷新候选窗。内联文字要等 TSF 的下一次 Sync
    // 才写回文档;候选窗若先变,空格上屏的就是用户没看到的那个。
    s.settle_pick = Some(SettlePick {
        decision,
        revision: job.snap.revision,
        seq: job.seq,
    });
}

/// `Sync` 时调用:停顿算出的首选仍然有效,就在这一刻与内联文字一起切过去。
pub(super) fn take_pick(s: &mut Session) {
    let Some(pick) = s.settle_pick.take() else {
        return;
    };
    // 展开棋盘期间不换首选,收起后再等下一次停顿。
    if s.expanded || s.context_seq != pick.seq {
        return;
    }
    if !result_is_current(
        s.engine.revision(),
        pick.revision,
        s.engine.page(),
        s.engine.highlight(),
        s.engine.composition().is_some(),
    ) {
        return;
    }
    let (changed, head_changed) = match pick.decision {
        Decision::Head(index) => (s.engine.promote(index), true),
        Decision::Order(order) => (s.engine.reorder_candidates(&order), order[0] != 0),
    };
    if changed {
        refresh_view(s);
        sync_pending(s);
        if head_changed {
            s.view.promotions = s.view.promotions.wrapping_add(1);
        }
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}
