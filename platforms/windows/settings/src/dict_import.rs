//! 词库导入:第三方词表 → 用户词库的原生格式。
//!
//! 一份词表里能混着三种写法,逐行认:
//! - 本机导出的用户词库(以 `w` `t` `g` `a` `p` 开头的记录行):原样并入,
//!   由引擎的导入校验。
//! - 标准词表(词 / 拼音 / 权重):搜狗、QQ、Rime 导出的文本词库。
//! - 纯汉字词表:每行一个词,读音从主词典反查;查不到就整条跳过,不猜。
//!
//! 词表里的拼音统一规范成引擎的学习键(`ni'hao`),权重取对数折算成使用次数:
//! 导入的词要压过普通候选,又不能把别的候选挤出候选窗。
//!
//! 解析与解码都在设置窗那侧做,常驻服务只收合并好的原生文本。

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use lueying_dict::Dictionary;
use lueying_dict::syllables::{canonical, is_syllable};

/// 词表里没有权重时的默认值;折算后是词频 10。
const DEFAULT_WEIGHT: u32 = 1000;

/// 最长的合法音节(`chuang` / `shuang`)。
const MAX_SYLL_LEN: usize = 6;

/// 一次导入的结果。
#[derive(Default)]
pub struct Report {
    /// 转成原生格式的词条行,每行 `键<TAB>词<TAB>频次<TAB>时间戳`。
    pub text: String,
    /// 并入的词条数。
    pub imported: usize,
    /// 跳过的行数。
    pub skipped: usize,
    /// 跳过里「只有汉字、没有读音」的行数。
    pub no_pinyin: usize,
    /// 只有汉字、等着从主词典查读音的词。
    pending: Vec<String>,
}

/// 解码词表文件:带 BOM 的按 BOM 走(UTF-8 / UTF-16),其余先按 UTF-8 读,
/// 失败按 GB18030 读(国内词表多是 GBK 系)。
pub fn decode(bytes: &[u8]) -> String {
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _, _) = encoding.decode(&bytes[bom..]);
        return text.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::GB18030.decode(bytes).0.into_owned(),
    }
}

/// 解析词表:逐行认格式,转成用户词库原生格式。
///
/// 纯汉字词表先攒着,交给 [`Report::resolve`] 从主词典补读音;补不上就整条跳过。
pub fn parse(text: &str) -> Report {
    let now = now_secs();
    let mut report = Report::default();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        // 本机导出的记录行,原样并入;格式由引擎的导入校验。
        if is_engine_line(&cols) {
            report.text.push_str(line);
            report.text.push('\n');
            continue;
        }
        // 标准词表:词 / 拼音 / 权重。
        if let Some((word, key, freq)) = standard_entry(&cols) {
            report.push(&key, &word, freq, now);
            continue;
        }
        // 纯汉字词表:读音等主词典给。
        if cols.len() == 1 && is_hanzi_word(cols[0].trim()) {
            report.pending.push(cols[0].trim().to_string());
            continue;
        }
        report.skipped += 1;
    }
    report
}

/// 本机导出的记录行:首列是记录类型(词、词序、模糊组、快捷字面、置顶)。
fn is_engine_line(cols: &[&str]) -> bool {
    matches!(cols.first(), Some(&("w" | "t" | "g" | "a" | "p")))
}

/// 标准词表的一行:词 / 拼音 / 权重。读音读不出来,或词与音节数对不上,就没有词条。
fn standard_entry(cols: &[&str]) -> Option<(String, String, u32)> {
    if cols.len() < 2 || cols[0].trim().is_empty() {
        return None;
    }
    let key = key_of(cols[1])?;
    let word = cols[0].trim();
    if !count_matches(word, &key) {
        return None;
    }
    let freq = weight_to_freq(cols.get(2).copied());
    Some((word.to_string(), key, freq))
}

impl Report {
    /// 给纯汉字词补读音:从主词典反查,查到就并入,查不到整条跳过。
    /// 没有主词典时这些行同样跳过。
    pub fn resolve(&mut self, dict: Option<&Dictionary>) {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        let Some(dict) = dict else {
            self.skipped += pending.len();
            self.no_pinyin += pending.len();
            return;
        };
        let wanted: HashSet<&str> = pending.iter().map(String::as_str).collect();
        // 主词典按键排序,同一个词的不同读音散在各处,取词频最高的那条。
        let mut found: HashMap<&str, (&str, u64)> = HashMap::new();
        for entry in dict.iter() {
            if !wanted.contains(entry.word) {
                continue;
            }
            let slot = found.entry(entry.word).or_insert((entry.key, entry.freq));
            if entry.freq > slot.1 {
                *slot = (entry.key, entry.freq);
            }
        }
        let now = now_secs();
        for word in pending {
            match found.get(word.as_str()) {
                Some((key, _)) => self.push(key, &word, weight_to_freq(None), now),
                None => {
                    self.skipped += 1;
                    self.no_pinyin += 1;
                }
            }
        }
    }

    /// 还有等着查读音的词吗。
    pub fn needs_dict(&self) -> bool {
        !self.pending.is_empty()
    }

    /// 追加一条词记忆:词表权重折算成的使用次数,稳定度取初始值,按现在记。
    fn push(&mut self, key: &str, word: &str, uses: u32, now: u64) {
        self.text
            .push_str(&format!("w\t{key}\t{word}\t{uses}\t1\t{now}\t0\n"));
        self.imported += 1;
    }
}

/// 按字节预算把原生格式切成若干块;按行切,单行超预算也照发。
pub fn split(text: &str, budget: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        at += line.len();
        if at - start >= budget {
            out.push(&text[start..at]);
            start = at;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// 主词典(exe 同目录的 main.pyd / main.tsv);给纯汉字词表反查读音用。
pub fn open_main_dict() -> Option<Dictionary> {
    for name in ["main.pyd", "main.tsv"] {
        let mut path = std::env::current_exe().ok()?;
        path.set_file_name(name);
        if !path.exists() {
            continue;
        }
        if let Ok((dict, _)) = Dictionary::from_path(&path)
            && !dict.is_empty()
        {
            return Some(dict);
        }
    }
    None
}

/// 词表里的拼音 → 引擎学习键(`ni'hao`);读不出来就没有键。
fn key_of(text: &str) -> Option<String> {
    // 声调数字(`ni3 hao3`)去掉,剩下的是同一个音节。
    let cleaned: String = text.chars().filter(|ch| !ch.is_ascii_digit()).collect();
    let lower = cleaned.to_ascii_lowercase();
    let parts: Vec<&str> = lower
        .split(|ch: char| ch.is_whitespace() || ch == '\'' || ch == ',' || ch == '-')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut syllables: Vec<String> = Vec::new();
    for part in parts {
        if is_syllable(part) {
            syllables.push(canonical(part).to_string());
        } else {
            syllables.extend(split_syllables(part)?);
        }
    }
    Some(syllables.join("'"))
}

/// 连续拼音串按最长音节贪心切分;切不开就没有键。
fn split_syllables(text: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let mut hit = None;
        let span = text.len().saturating_sub(at).min(MAX_SYLL_LEN);
        for len in (1..=span).rev() {
            if !text.is_char_boundary(at + len) {
                continue;
            }
            let piece = &text[at..at + len];
            if is_syllable(piece) {
                hit = Some(len);
                break;
            }
        }
        let len = hit?;
        out.push(canonical(&text[at..at + len]).to_string());
        at += len;
    }
    Some(out)
}

/// 词的字数与音节数对不上就进不了候选,这种行直接跳过。
fn count_matches(word: &str, key: &str) -> bool {
    !is_hanzi_word(word) || word.chars().count() == key.split('\'').count()
}

/// 文件权重 → 使用次数:取对数(权重 1000 → 10 次)。
fn weight_to_freq(weight: Option<&str>) -> u32 {
    let weight = weight
        .and_then(|text| text.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_WEIGHT);
    weight.checked_ilog2().unwrap_or(0) + 1
}

/// 是否整词都是汉字。
fn is_hanzi_word(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|ch| {
            matches!(ch as u32,
                0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
                | 0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x30000..=0x323AF)
        })
}

/// 当前时间(秒);导入的词条按现在记,不会被当成陈旧记录。
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "dict_import_tests.rs"]
mod tests;
