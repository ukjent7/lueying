//! 词后表情候选:按候选词查 emoji 别名表,在词后插入少量表情。
//!
//! 数据来自 rime-frost 的 `opencc/emoji.txt`。表情不是词:不学习、不造词,
//! 也不参与模型重排,只借候选词的 consume/spans 上屏。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::query::Candidate;

/// 只给前几条候选配表情,避免整页都是表情。
const SCAN: usize = 6;
/// 一次组合最多插入几个表情。
const MAX_TOTAL: usize = 8;
/// 同一个词最多带几个表情。
const MAX_PER_WORD: usize = 3;
/// 单字不插表情:单字候选常靠数字选同音字,插入表情会挤乱数字选词;
/// 两字及以上的词才跟随表情。
const MIN_WORD_CHARS: usize = 2;

pub(crate) fn inject(cands: &mut Vec<Candidate>) {
    if cands.is_empty() {
        return;
    }
    // 单字/固定候选都不插表情;没有可插的候选就不必加载别名表。
    let eligible = cands
        .iter()
        .take(SCAN)
        .any(|cand| !cand.quick && !cand.no_learn && cand.text.chars().count() >= MIN_WORD_CHARS);
    if !eligible {
        return;
    }
    let table = table();
    let mut out = Vec::with_capacity(cands.len() + MAX_TOTAL);
    let mut pending: Vec<Candidate> = Vec::new();
    let mut added = 0;
    // 打断块:快捷写法(日期/时间)和吃不满输入的分段入口。表情要排在这些
    // 候选之后,否则「你好」的表情会占住下移一格的位置,「时间」的 14:05
    // 也会被表情顶到第三位。
    let top_cut = cands
        .first()
        .and_then(|cand| cand.spans.last())
        .map_or(0, |span| span.1);
    let last_barrier = cands
        .iter()
        .enumerate()
        .filter(|(_, cand)| {
            cand.quick || cand.spans.last().map_or(0, |span| span.1) < top_cut
        })
        .map(|(index, _)| index)
        .max();
    for (index, cand) in std::mem::take(cands).into_iter().enumerate() {
        if last_barrier.is_none_or(|barrier| index > barrier) && !pending.is_empty() {
            out.append(&mut pending);
        }
        let emojis = if index < SCAN
            && added < MAX_TOTAL
            && !cand.quick
            && !cand.no_learn
            && cand.text.chars().count() >= MIN_WORD_CHARS
        {
            table.get(cand.text.as_str()).copied()
        } else {
            None
        };
        let base = cand.clone();
        out.push(cand);
        let Some(emojis) = emojis else {
            continue;
        };
        for (offset, emoji) in emojis.split_whitespace().take(MAX_PER_WORD).enumerate() {
            if added >= MAX_TOTAL {
                break;
            }
            let mut extra = base.clone();
            extra.text = emoji.to_string();
            extra.words.clear();
            extra.full = false;
            extra.trusted = false;
            extra.sentence = false;
            extra.edits = 0;
            extra.freq = 0;
            extra.user_freq = 0;
            extra.learned = false;
            extra.groups = 0;
            extra.hint = Some("表情".to_string());
            extra.learn_key = None;
            extra.quick = false;
            extra.rejected = false;
            extra.learn_text = None;
            extra.no_learn = true;
            extra.completion = false;
            extra.score = base.score - (offset as f64 + 1.0) * 1.0e-6;
            pending.push(extra);
            added += 1;
        }
    }
    out.append(&mut pending);
    *cands = out;
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../data/emoji.tsv")
            .lines()
            .filter_map(|line| {
                let (alias, emojis) = line.split_once('\t')?;
                (!alias.is_empty() && !emojis.is_empty()).then_some((alias, emojis))
            })
            .collect()
    })
}
