//! 输入会话的跨平台内核。
//!
//! Windows 的 TSF 宿主与 macOS 的 InputMethodKit 外壳各自管按键怎么进来、
//! 文本怎么写回去;会话里"引擎状态怎么变成候选视图""重排要拿什么输入"
//! "两档模型怎么决策"三件事两边一样,放在这里共用。
//!
//! 后台线程用独立的推理状态处理 [`snapshot`] 复制出的快照,无需持有会话锁。

#![deny(missing_docs)]

use crate::{Engine, Mode};
use lueying_protocol::SessionView;
use lueying_rerank::{ContextRank, Fact, Rerank};

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
    /// 确认仍紧邻当前输入的上屏词;只有宿主原文、没有已知词界时留空。
    pub adjacent_word: Option<String>,
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
        view.preedit = comp.raw.clone();
    } else {
        view.raw.clear();
        view.preedit.clear();
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
    let context = engine.context();
    let adjacent_word = engine
        .last_word
        .as_deref()
        .filter(|word| !word.is_empty() && context.ends_with(word))
        .or_else(|| {
            engine
                .last_commit
                .as_ref()
                .filter(|anchor| anchor.matches_tail(context))
                .map(|anchor| anchor.word())
        })
        .map(str::to_owned);
    Some(Snapshot {
        context: context.to_string(),
        adjacent_word,
        raw: comp.raw.clone(),
        revision: engine.revision(),
        texts: candidates.iter().map(|c| c.text.clone()).collect(),
        facts: candidates
            .iter()
            .map(|c| Fact {
                answers_key: c.full
                    && !c.rejected
                    && !c.no_learn
                    && !c.weaker_reading
                    && !c.needs_clause_prefix,
                trusted_dictionary_hit: c.trusted,
                score: c.score as f32,
                // 信道代价按学习前的原始读法报:模糊组减价是用户习惯,对模型应
                // 如实报告读法改动量,否则习惯分会被重排目标里的 −CHANNEL_WEIGHT
                // 放大成「这条模糊读法很干净」,在没见过的句子上劫持首选。
                channel: (c.channel + c.relief) as f32,
                personal: c.personal as f32,
                locked: c.learned_enough() || c.attested_context,
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

/// 有上文时重排多个候选,不挪动未参与比较的槽位;相邻词会再做连接/断句对照。
pub fn rank_context(rerank: &mut Rerank, snapshot: &Snapshot) -> Vec<usize> {
    rank_context_with_gap(rerank, snapshot).order
}

/// 同次重排附带前两名分差;缺少可比候选时不报告置信度。
pub fn rank_context_with_gap(rerank: &mut Rerank, snapshot: &Snapshot) -> ContextRank {
    let texts: Vec<&str> = snapshot.texts.iter().map(String::as_str).collect();
    rerank.rank_with_local_context(
        &snapshot.context,
        &texts,
        &snapshot.facts,
        snapshot.adjacent_word.as_deref(),
    )
}

/// [`rank_context`] 的预筛版:停顿决策由小模型(每键同步档)先从更深的
/// 候选池里挑出比选窗口,再由本模型裁决。
pub fn rank_context_prefiltered(
    rerank: &mut Rerank,
    prefilter: &mut Rerank,
    snapshot: &Snapshot,
) -> Vec<usize> {
    let texts: Vec<&str> = snapshot.texts.iter().map(String::as_str).collect();
    rerank
        .rank_with_local_context_prefiltered(
            &snapshot.context,
            &texts,
            &snapshot.facts,
            snapshot.adjacent_word.as_deref(),
            Some(prefilter),
        )
        .order
}

/// 双模型的选择性停顿门槛。没有有效分差时仍允许原来的 25M 回退。
pub fn can_skip_25m(snapshot: &Snapshot, ranking: &ContextRank) -> bool {
    const AMBIGUOUS_GAP: f32 = 2.0;
    ranking.gap.is_some_and(|gap| {
        gap.is_finite()
            && (gap > AMBIGUOUS_GAP
                || !ranking
                    .order
                    .iter()
                    .take(3)
                    .any(|&i| snapshot.facts[i].edited))
    })
}

/// 25M 停顿重排:模型直接给出最终顺序,没有上下文也能用。
pub fn decide_settled(rerank: &mut Rerank, snapshot: &Snapshot) -> Option<usize> {
    let texts: Vec<&str> = snapshot.texts.iter().map(String::as_str).collect();
    rerank.decide_settled_with_local_context(
        &snapshot.context,
        &texts,
        &snapshot.facts,
        snapshot.adjacent_word.as_deref(),
    )
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
    fn selective_25m_requires_both_a_close_score_and_a_likely_correction() {
        let mut snap = Snapshot {
            facts: vec![Fact::default(); 4],
            ..Snapshot::default()
        };
        snap.facts[3].edited = true;
        let mut rank = ContextRank {
            order: vec![0, 1, 2, 3],
            gap: Some(2.0),
        };
        assert!(can_skip_25m(&snap, &rank));
        rank.order.swap(2, 3);
        assert!(!can_skip_25m(&snap, &rank));
        rank.gap = Some(2.01);
        assert!(can_skip_25m(&snap, &rank));
        for gap in [None, Some(f32::NAN)] {
            rank.gap = gap;
            assert!(!can_skip_25m(&snap, &rank));
        }
    }

    #[test]
    fn plain_composition_has_no_pinned_candidate() {
        assert!(!pinned_candidates(&composed()));
    }

    #[test]
    fn reorder_moves_only_scored_slots_and_space_commits_the_new_head() {
        let mut engine = composed();
        let mut fixed = crate::query::Candidate::whole(5, "固定", 0.0);
        fixed.no_learn = true;
        engine.cands = vec![
            crate::query::Candidate::whole(5, "甲", 0.0),
            fixed,
            crate::query::Candidate::whole(5, "乙", 0.0),
            crate::query::Candidate::whole(5, "丙", 0.0),
        ];
        let before = engine.revision();
        assert!(!engine.reorder_candidates(&[2, 1, 0]));
        assert!(!engine.reorder_candidates(&[2, 0, 0]));
        assert_eq!(engine.revision(), before);
        assert!(engine.reorder_candidates(&[3, 0, 2]));
        assert_eq!(engine.candidates(), ["丙", "固定", "甲", "乙"]);
        assert_ne!(engine.revision(), before);
        assert!(
            engine
                .process(Key::Space)
                .into_iter()
                .any(|event| matches!(event, crate::Event::Commit(text) if text == "丙"))
        );
    }
    #[test]
    fn mixed_input_keeps_the_complete_syllable_during_automatic_ranking() {
        use crate::Event;
        let words = "改一下\t1\tgai'yi'xia\n关爱\t50000\tguan'ai\n一下\t50000\tyi'xia\n";
        let typed = |keys: &str| {
            let mut engine =
                Engine::new(Arc::new(Dictionary::from_text(words).0), Config::default());
            engine.set_context("我们要多");
            for key in keys.chars() {
                engine.process(if key == '\'' {
                    Key::Punct(key)
                } else {
                    Key::Letter(key)
                });
            }
            engine
        };
        let mut engine = typed("gaiyx");
        let snap = snapshot(&engine).unwrap();
        assert_eq!(snap.texts[0], "改一下");
        assert!(snap.facts[0].answers_key);
        let weaker = snap
            .texts
            .iter()
            .position(|s| s == "关爱一下")
            .expect("仍可选择的次级读法");
        assert!(engine.candidates_detailed()[weaker].full);
        assert!(!snap.facts[weaker].answers_key);
        assert!(!engine.promote(weaker));
        assert!(!engine.reorder_candidates(&[weaker, 0]));
        assert_eq!(engine.pick(weaker), [Event::Commit("关爱一下".into())]);
        let explicit = snapshot(&typed("g'aiyx")).unwrap();
        assert_eq!(explicit.raw, "g'aiyx");
        let index = explicit.texts.iter().position(|s| s == "关爱一下").unwrap();
        assert!(explicit.facts[index].answers_key);
    }
    #[test]
    fn a_fresh_clause_prefers_openers_but_particles_remain_selectable() {
        let words = "吧\t9000\tba\n把\t3000\tba\n不\t15000\tbu\n吗\t7000\tma\n妈\t3000\tma\n啊\t7000\ta\n阿\t3000\ta\n呢\t1000\tne\n";
        let (dict, bad) = Dictionary::from_text(words);
        assert_eq!(bad, 0);
        let dict = Arc::new(dict);
        let typed = |context: &str, keys: &str| {
            let mut engine = Engine::new(Arc::clone(&dict), Config::default());
            engine.set_context(context);
            for key in keys.chars() {
                engine.process(Key::Letter(key));
            }
            engine
        };
        for context in ["", "上句。", "上句！ “", "上句\n  "] {
            let mut engine = typed(context, "ba");
            let snap = snapshot(&engine).unwrap();
            assert_eq!(snap.texts[0], "把");
            let particle = snap.texts.iter().position(|s| s == "吧").unwrap();
            assert!(!snap.facts[particle].answers_key);
            assert!(!engine.promote(particle));
            assert_eq!(engine.pick(particle), [crate::Event::Commit("吧".into())]);
        }
        for (context, keys, expected) in [
            ("", "b", "不"),
            ("", "ma", "妈"),
            ("", "a", "啊"),
            ("", "ne", "呢"),
            ("我们走", "ba", "吧"),
            ("可以", "ma", "吗"),
        ] {
            let engine = typed(context, keys);
            let snap = snapshot(&engine).unwrap();
            assert_eq!(snap.texts[0], expected);
            assert!(snap.facts[0].answers_key);
        }
    }
}
