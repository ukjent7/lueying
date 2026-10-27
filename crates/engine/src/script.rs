//! 简体 → 繁体输出(OpenCC `s2t` 的最大正向匹配),可再叠地区字形。
//!
//! 基础数据来自 OpenCC 的 `STCharacters` / `STPhrases`:只保留单字表无法正确
//! 覆盖的短语(头发、干净、皇后、面条等),运行时先查最长短语再退回单字。
//! 台湾/香港字形在通用转换之上二次映射,数据取 OpenCC ver.1.1.9 的
//! `TWVariants` / `HKVariants`(多值条目取首选,异体同形条目剔除),只改
//! 异体字,不做词汇替换。转换只改显示与上屏;学习与上下文仍用简体规范
//! 形式,避免简繁分裂词库。OpenCC 数据按 Apache-2.0 许可,见
//! `licenses/Apache-2.0.txt`。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::types::Script;

/// OpenCC `STPhrases` 最长条目为 15 个汉字。
const MAX_PHRASE_CHARS: usize = 15;

/// 按配置字形转换;简体原样返回。
pub(crate) fn convert(text: &str, script: Script) -> String {
    if text.is_ascii() {
        return text.to_string();
    }
    let base = to_traditional(text);
    match script {
        Script::Simplified | Script::Traditional => base,
        Script::Taiwan => remap(&base, tw_chars()),
        Script::HongKong => remap(&base, hk_chars()),
    }
}

fn tw_chars() -> &'static HashMap<char, char> {
    static TW: OnceLock<HashMap<char, char>> = OnceLock::new();
    TW.get_or_init(|| regional_chars(include_str!("../data/s2t_tw.tsv")))
}

fn hk_chars() -> &'static HashMap<char, char> {
    static HK: OnceLock<HashMap<char, char>> = OnceLock::new();
    HK.get_or_init(|| regional_chars(include_str!("../data/s2t_hk.tsv")))
}

/// 地区字形二次映射:变体表全是单字,逐字替换即可。
fn remap(text: &str, chars: &HashMap<char, char>) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        out.push(chars.get(&c).copied().unwrap_or(c));
    }
    out
}

/// 变体表条目全是单字对。
fn regional_chars(data: &'static str) -> HashMap<char, char> {
    data.lines()
        .filter_map(|line| {
            let (from, to) = line.split_once('\t')?;
            let mut from_chars = from.chars();
            let from = from_chars.next()?;
            let mut to_chars = to.chars();
            let to = to_chars.next()?;
            (from_chars.next().is_none() && to_chars.next().is_none()).then_some((from, to))
        })
        .collect()
}

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
