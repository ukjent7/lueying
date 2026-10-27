//! 中文态英文混输:非拼音串给出英文词候选,保留专名大小写。
//!
//! 只补一个候选,不改中文排序。英文不是用户词:不学习、不造词,也不参与
//! 模型重排。半截拼音已有中文候选时不出原文,避免 `nih` 把内联显示顶成字母。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::query::Candidate;

pub(crate) fn candidate(raw: &str, has_full: bool, has_candidates: bool) -> Option<Candidate> {
    if raw.len() < 2 || !raw.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    let text = match table().get(raw) {
        Some(text) => *text,
        None if has_full || has_candidates => return None,
        None => raw,
    };
    let mut cand = Candidate::whole(raw.len(), text, 0.0);
    cand.words.clear();
    cand.full = false;
    cand.hint = Some("英文".to_string());
    cand.no_learn = true;
    Some(cand)
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../data/en.tsv")
            .lines()
            .filter_map(|line| {
                let (key, word) = line.split_once('\t')?;
                (!key.is_empty() && !word.is_empty()).then_some((key, word))
            })
            .collect()
    })
}
