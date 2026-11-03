//! 8M 按键重排:按键只取一份快照,推理与候选提升都在后台线程完成。
//!
//! 结果落地前核对 revision/页/高亮:输入已经往前走了就丢弃。按键路径因此
//! 只花引擎本身的时间,模型快慢不再决定一次按键的延迟。

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, mpsc};

use lueying_rerank::settle::result_is_current;

use crate::imp::{Session, State};

/// 一次按键重排的最小输入;`rerank` 随会话存活,前缀缓存跨按键复用。
pub(super) struct Job {
    session: u64,
    /// 发起时的焦点/上下文代次;中途换了输入范围就丢弃。
    seq: u64,
    /// 重排输入快照;上下文、候选与版本号都在里面。
    snap: lueying_session::Snapshot,
    rerank: Arc<Mutex<lueying_rerank::Rerank>>,
}

/// 按键或上下文变化后调用:门控不通过就什么也不做,永不阻塞调用方。
pub(super) fn schedule(state: &State, session: u64, s: &Session) {
    let Some(tx) = state.sync_tx.as_ref() else {
        return;
    };
    let Some(rerank) = s.sync_rerank.as_ref() else {
        return;
    };
    // 快捷候选固定在本轮插入的位置,8M 模型不能把它们换走;置顶词同理。
    if lueying_session::pinned_candidates(&s.engine) {
        return;
    }
    let Some(snap) = lueying_session::snapshot(&s.engine) else {
        return;
    };
    // 没有左邻上下文时,8M 只能按句子自身打分:共享语料上它的排序比引擎
    // 低 7.6 个百分点,而停顿重排的最终结果与是否先跑 8M 完全相同。
    if !lueying_session::sync_worthwhile(&snap) {
        return;
    }
    // 通道满说明上一键的结果还没出来:直接丢掉这一次,排队只会让结果更旧。
    let _ = tx.try_send(Job {
        session,
        seq: s.context_seq,
        snap,
        rerank: Arc::clone(rerank),
    });
}

pub(super) fn spawn(state: Arc<State>, rx: mpsc::Receiver<Job>) {
    std::thread::Builder::new()
        .name("lueying-sync-rerank".into())
        .spawn(move || {
            for job in rx {
                // 一次推理 panic 不能让整条线程退出;丢掉本次结果即可。
                let _ = catch_unwind(AssertUnwindSafe(|| run(&state, &job)));
            }
        })
        .expect("启动按键重排线程失败");
}

fn run(state: &State, job: &Job) {
    let promote = lueying_session::decide_sync(&job.rerank, &job.snap);
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
        crate::imp::refresh_view(&mut s);
        crate::imp::sync_pending(&mut s);
        if handle.publish(&s) {
            crate::imp::touch_view(state);
        }
    }
}
