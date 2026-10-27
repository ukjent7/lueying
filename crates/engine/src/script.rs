//! 简体 → 繁体输出(OpenCC `s2t` 的最大正向匹配)。
//!
//! 数据由 OpenCC 的 `STCharacters` / `STPhrases` 生成:只保留单字表无法正确
//! 覆盖的短语(头发、干净、皇后、面条等),运行时先查最长短语再退回单字。
//! 转换只改显示与上屏;学习与上下文仍用简体规范形式,避免简繁分裂词库。

use std::collections::HashMap;
use std::sync::OnceLock;

/// OpenCC `STPhrases` 最长条目为 15 个汉字。
const MAX_PHRASE_CHARS: usize = 15;

pub(crate) fn to_traditional(text: &str) -> String {
    if text.is_ascii() {
        return text.to_string();
    }
    let chars = char_table();
    let phrases = phrase_table();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        if let Some((end, value)) = longest_phrase(text, at, phrases) {
            out.push_str(value);
            at = end;
        } else if let Some(c) = text[at..].chars().next() {
            out.push(chars.get(&c).copied().unwrap_or(c));
            at += c.len_utf8();
        } else {
            break;
        }
    }
    out
}

fn longest_phrase<'a>(
    text: &str,
    at: usize,
    phrases: &'a HashMap<&'static str, &'static str>,
) -> Option<(usize, &'a str)> {
    let mut ends = [0usize; MAX_PHRASE_CHARS];
    let mut count = 0;
    for (offset, c) in text[at..].char_indices() {
        if count == MAX_PHRASE_CHARS {
            break;
        }
        ends[count] = at + offset + c.len_utf8();
        count += 1;
    }
    for &end in ends[..count].iter().rev() {
        if let Some(value) = phrases.get(&text[at..end]).copied() {
            return Some((end, value));
        }
    }
    None
}

fn char_table() -> &'static HashMap<char, char> {
    static TABLE: OnceLock<HashMap<char, char>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../data/s2t_chars.tsv")
            .lines()
            .filter_map(|line| {
                let (from, to) = line.split_once('\t')?;
                let mut from_chars = from.chars();
                let from = from_chars.next()?;
                let mut to_chars = to.chars();
                let to = to_chars.next()?;
                (from_chars.next().is_none() && to_chars.next().is_none()).then_some((from, to))
            })
            .collect()
    })
}

fn phrase_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../data/s2t_phrases.tsv")
            .lines()
            .filter_map(|line| {
                let (from, to) = line.split_once('\t')?;
                (!from.is_empty() && !to.is_empty()).then_some((from, to))
            })
            .collect()
    })
}
