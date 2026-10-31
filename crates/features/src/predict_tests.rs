//! 外部联想来源的合并规则:位置、去重、拼音前缀过滤与数量上限。

use super::{PredictSource, merge_external};
use lueying_ext::Candidate;

/// 固定词表来源;predict 原样吐出前 `limit` 条。
struct Fixed(Vec<(String, String)>);

impl PredictSource for Fixed {
    fn predict(&self, _previous: &[&str], limit: usize) -> Vec<(String, String)> {
        self.0.iter().take(limit).cloned().collect()
    }
}

#[test]
fn external_suggestions_land_after_builtin() {
    let mut cands = vec![Candidate::whole(2, "你好".to_string(), 1.0e12)];
    let inserted = merge_external(
        &mut cands,
        2,
        "ni",
        vec![("你号".into(), "ni'hao".into()), ("昵称".into(), "nicheng".into())],
        0,
    );
    assert_eq!(inserted, 2);
    // 首位仍是核心候选;联想词按序压在后面。
    assert_eq!(cands[0].text, "你好");
    assert_eq!(cands[1].text, "你号");
    assert_eq!(cands[2].text, "昵称");
    assert!(cands[1].completion);
    assert_eq!(cands[1].hint.as_deref(), Some("联想"));
    // 外部联想压在内置联想(1.0e8)之下。
    assert!(cands[1].score < 1.0e8);
    assert!(cands[2].score < cands[1].score);
}

#[test]
fn dedupes_against_core_candidates() {
    let mut cands = vec![
        Candidate::whole(2, "你好".to_string(), 1.0e12),
        Candidate::whole(2, "你号".to_string(), 1.0e6),
    ];
    let inserted = merge_external(
        &mut cands,
        2,
        "ni",
        vec![("你号".into(), "ni'hao".into()), ("昵称".into(), "nicheng".into())],
        0,
    );
    assert_eq!(inserted, 1, "已有的词不再补一条");
    assert!(!cands.iter().skip(1).any(|cand| cand.text == "你号" && cand.completion));
}

#[test]
fn pinyin_must_prefix_match_the_query() {
    let mut cands = Vec::new();
    let inserted = merge_external(
        &mut cands,
        2,
        "ni",
        vec![("学习".into(), "xuexi".into()), ("那里".into(), "na'li".into())],
        0,
    );
    assert_eq!(inserted, 1, "只有 na'li 前缀匹配 ni");
    assert_eq!(cands[0].text, "那里");
}

#[test]
fn caps_at_max_suggestions() {
    let mut cands = Vec::new();
    let externals: Vec<(String, String)> = (0..8)
        .map(|index| (format!("词{index}"), format!("ci{index}")))
        .collect();
    let inserted = merge_external(&mut cands, 3, "ci", externals, 0);
    assert_eq!(inserted, super::MAX_SUGGESTIONS);
    assert_eq!(cands.len(), super::MAX_SUGGESTIONS);
}

#[test]
fn fixed_source_respects_limit() {
    let source = Fixed(vec![
        ("你号".into(), "ni'hao".into()),
        ("昵称".into(), "nicheng".into()),
    ]);
    assert_eq!(source.predict(&["你好"], 1).len(), 1);
    assert_eq!(source.predict(&[], 2).len(), 2);
}
