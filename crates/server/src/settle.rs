//! 25M 停顿重排:按键后只记一个每会话待办,静默 120ms 才跑模型;
//! 推理不持有 `sessions` 锁,回来时用 revision/页/高亮确认结果仍然有效。

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use pinyin_engine::Engine;
use pinyin_rerank::Fact;
use pinyin_rerank::debounce::Debounce;
use pinyin_rerank::settle::result_is_current;

use crate::imp::{Session, State, refresh_view};

const DELAY_MS: u64 = 120;

pub(super) enum SettleMessage {
    Job(SettleJob),
    Cancel { session: u64 },
}

pub(super) struct SettleJob {
    session: u64,
    revision: u64,
    context: String,
    texts: Vec<String>,
    facts: Vec<Fact>,
    rerank: Arc<Mutex<pinyin_rerank::Rerank>>,
}

pub(super) struct Snapshot {
    pub(super) context: String,
    pub(super) revision: u64,
    pub(super) texts: Vec<String>,
    pub(super) facts: Vec<Fact>,
}

/// 候选及其事实的一次快照;没有组合时返回 `None`。
pub(super) fn snapshot(engine: &Engine) -> Option<Snapshot> {
    engine.composition()?;
    let cands = engine.candidates_detailed();
    Some(Snapshot {
        context: engine.context().to_string(),
        revision: engine.revision(),
        texts: cands.iter().map(|c| c.text.clone()).collect(),
        facts: cands
            .iter()
            .map(|c| Fact {
                answers_key: c.full,
                trusted_dictionary_hit: c.trusted,
            })
            .collect(),
    })
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
    let Some(snap) = snapshot(&s.engine) else {
        let _ = tx.send(SettleMessage::Cancel { session });
        return;
    };
    let _ = tx.send(SettleMessage::Job(SettleJob {
        session,
        revision: snap.revision,
        context: snap.context,
        texts: snap.texts,
        facts: snap.facts,
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
        .name("pinyin-settle".into())
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
                    let promote = {
                        let mut rerank = job.rerank.lock().unwrap_or_else(|p| p.into_inner());
                        let texts: Vec<&str> = job.texts.iter().map(String::as_str).collect();
                        rerank.decide(&job.context, &texts, &job.facts)
                    };
                    apply(&state, &job, promote);
                }
            }
        })
        .expect("启动停顿重排线程失败");
}

fn apply(state: &State, job: &SettleJob, promote: Option<usize>) {
    let Some(index) = promote else {
        return;
    };
    let mut sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let Some(s) = sessions.get_mut(&job.session) else {
        return;
    };
    if !result_is_current(
        s.engine.revision(),
        job.revision,
        s.engine.page(),
        s.engine.highlight(),
        s.engine.composition().is_some(),
    ) {
        return;
    }
    if s.engine.promote(index) {
        refresh_view(s);
        state.version.fetch_add(1, Ordering::Relaxed);
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}
