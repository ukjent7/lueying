//! 输入会话的跨平台内核。
//!
//! Windows 的 TSF 宿主与 macOS 的 InputMethodKit 外壳各自管按键怎么进来、
//! 文本怎么写回去;会话里"引擎状态怎么变成候选视图""重排要拿什么输入"
//! "两档模型怎么决策"三件事两边一样,放在这里共用。
//!
//! 重排推理要跑在后台线程上,所以 [`decide_sync`] 与 [`decide_settled`] 只
//! 吃一把锁:调用方先把 [`snapshot`] 拿到的输入复制出来,推理时不必占着
//! 会话锁。

#![deny(missing_docs)]

use std::sync::Mutex;

use crate::{Engine, Mode};
use lueying_protocol::SessionView;
use lueying_rerank::{Fact, Rerank};

/// 光标附近保留的字符数;更长的拼音串只留这一段。
const WINDOW: usize = 12;

/// 重排要用的输入快照;没有组合时是 `None`。
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// 左邻上下文;空串表示没有可用上下文。
    pub context: String,
    /// 组合按键原始串。
    pub raw: String,
    /// 发起时的引擎版本号,用来判断推理结果是否已经过期。
    pub revision: u64,
    /// 候选文本。
    pub texts: Vec<String>,
    /// 与候选一一对应的事实。
    pub facts: Vec<Fact>,
}

/// 把引擎状态同步成候选视图。
///
/// 光标可在拼音串中移动:编辑时在光标处插标记,长串只留光标附近一段,
/// 免得标记被候选窗的单行截断。光标在末尾时不插标记,只留末尾一段。
pub fn refresh(engine: &Engine, view: &mut SessionView) {
    if let Some(comp) = engine.composition() {
        let cursor = comp.cursor.min(comp.raw.len());
        let len = comp.raw.len();
        let mut raw = String::new();
        if cursor < len {
            let start = cursor.saturating_sub(WINDOW);
            let end = (cursor + WINDOW).min(len);
            if start > 0 {
                raw.push('…');
            }
            raw.push_str(&comp.raw[start..cursor]);
            raw.push('|');
            raw.push_str(&comp.raw[cursor..end]);
            if end < len {
                raw.push('…');
            }
        } else if len <= WINDOW * 2 {
            raw.push_str(&comp.raw);
        } else {
            raw.push('…');
            raw.push_str(&comp.raw[len - WINDOW * 2..]);
        }
        view.raw = raw;
        view.preedit = comp.display.clone();
        view.segments = comp
            .seg_spans
            .iter()
            .map(|(start, end)| comp.raw[*start..*end].to_string())
            .collect();
    } else {
        view.raw.clear();
        view.preedit.clear();
        view.segments.clear();
    }
    let (texts, hints): (Vec<String>, Vec<String>) = engine
        .candidates_detailed()
        .iter()
        .map(|candidate| {
            (
                candidate.text.clone(),
                candidate.hint.clone().unwrap_or_default(),
            )
        })
        .unzip();
    view.candidates = texts;
    view.hints = hints;
    view.page = engine.page();
    view.page_size = engine.page_size();
    view.highlight = engine.highlight();
    view.chinese = engine.mode() == Mode::Chinese;
    view.fullwidth = engine.fullwidth();
}

/// 候选及其事实的一次快照;没有组合时返回 `None`。
pub fn snapshot(engine: &Engine) -> Option<Snapshot> {
    let comp = engine.composition()?;
    let candidates = engine.candidates_detailed();
    Some(Snapshot {
        context: engine.context().to_string(),
        raw: comp.raw.clone(),
        revision: engine.revision(),
        texts: candidates.iter().map(|c| c.text.clone()).collect(),
        facts: candidates
            .iter()
            .map(|c| Fact {
                answers_key: c.full,
                trusted_dictionary_hit: c.trusted,
                score: c.score as f32,
                channel: c.channel as f32,
                personal: c.personal as f32,
                locked: c.learned_enough(),
                edited: c.edits > 0,
            })
            .collect(),
    })
}

/// 快捷候选与置顶词固定在插入位置,重排不能把它们换走:换走了空格上屏的
/// 就会是另一个词。
pub fn pinned_candidates(engine: &Engine) -> bool {
    engine
        .candidates_detailed()
        .iter()
        .any(|candidate| candidate.quick || candidate.pinned)
}

/// 8M 按键重排只在有左邻上下文时才有意义:没有上下文时它只能按句子自身
/// 打分,共享语料上比引擎原序弱 7.6 个百分点。
pub fn sync_worthwhile(snapshot: &Snapshot) -> bool {
    !snapshot.context.is_empty()
}

/// 8M 按键重排:返回要提到首位的候选下标。
pub fn decide_sync(rerank: &Mutex<Rerank>, snapshot: &Snapshot) -> Option<usize> {
    if !sync_worthwhile(snapshot) {
        return None;
    }
    let texts: Vec<&str> = snapshot.texts.iter().map(String::as_str).collect();
    rerank
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .decide(&snapshot.context, &texts, &snapshot.facts)
}

/// 25M 停顿重排:模型直接给出最终顺序,没有上下文也能用。
pub fn decide_settled(rerank: &Mutex<Rerank>, snapshot: &Snapshot) -> Option<usize> {
    let texts: Vec<&str> = snapshot.texts.iter().map(String::as_str).collect();
    rerank
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .decide_settled(&snapshot.context, &texts, &snapshot.facts)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{Config, Engine, Key};
    use lueying_dict::Dictionary;

    use super::*;

    fn engine() -> Engine {
        Engine::new(Arc::new(Dictionary::builtin()), Config::default())
    }

    fn composed() -> Engine {
        let mut engine = engine();
        for key in ['n', 'i', 'h', 'a', 'o'] {
            engine.process(Key::Letter(key));
        }
        engine
    }

    #[test]
    fn refresh_fills_the_view() {
        let engine = composed();
        let mut view = SessionView::default();
        refresh(&engine, &mut view);
        assert_eq!(view.raw, "nihao");
        assert!(!view.preedit.is_empty(), "内联文本应当有内容");
        let expected = engine.candidates_detailed().len();
        assert_eq!(view.candidates.len(), expected);
        assert_eq!(view.hints.len(), expected);
        assert!(view.chinese);
    }

    #[test]
    fn refresh_clears_the_view_without_composition() {
        let engine = engine();
        let mut view = SessionView::default();
        refresh(&engine, &mut view);
        assert!(view.raw.is_empty());
        assert!(view.preedit.is_empty());
        assert!(view.candidates.is_empty());
    }

    #[test]
    fn snapshot_needs_a_composition() {
        assert!(snapshot(&engine()).is_none());
        let engine = composed();
        let snap = snapshot(&engine).expect("有组合");
        let expected = engine.candidates_detailed().len();
        assert_eq!(snap.texts.len(), expected);
        assert_eq!(snap.facts.len(), expected);
    }

    #[test]
    fn sync_needs_context() {
        let mut snap = Snapshot::default();
        assert!(!sync_worthwhile(&snap));
        snap.context = "今天".into();
        assert!(sync_worthwhile(&snap));
    }

    #[test]
    fn plain_composition_has_no_pinned_candidate() {
        assert!(!pinned_candidates(&composed()));
    }
}
