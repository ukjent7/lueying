//! 词典:文本格式加载、按键排序与精确 / 签名查询。
//!
//! 行格式 `词<TAB>词频<TAB>拼音音节`,音节以 `'` 分隔(`ni'hao`)。
//! `signature` 把键折叠成"每音节首字母、zh/ch/sh 归并为 z/c/s"的简拼签名,
//! 简拼与混合输入经签名索引一次定位到连续区段,再由引擎逐音节过滤。

pub mod syllables;

use std::collections::HashSet;
use std::path::Path;

pub use syllables::Fuzzy;

/// 一条词条。
#[derive(Clone, Debug)]
pub struct Entry {
    pub word: String,
    /// 音节键,`'` 分隔。
    pub key: String,
    pub freq: u64,
}

/// 已排序的词典。`entries` 按 (key, freq 逆序, word) 排;`sig_index` 按签名排序的条目下标。
pub struct Dictionary {
    entries: Vec<Entry>,
    sig_index: Vec<(String, usize)>,
}

/// 词的每个字符都要是 CJK 汉字(白霜数据里偶有符号词条,直接丢弃)。
fn is_hanzi(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|c| {
            matches!(c as u32,
                0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF)
        })
}

/// 键的音节签名:每音节取首字母(zh/ch/sh 的首字母恰好就是折叠后的 z/c/s)。
pub fn signature(key: &str) -> String {
    key.split('\'')
        .filter_map(|s| s.as_bytes().first().map(|&b| b as char))
        .collect()
}

impl Dictionary {
    /// 解析文本词典。格式或音节不合法的行跳过并计数;同 (word, key) 保留最大词频。
    pub fn from_text(text: &str) -> (Dictionary, usize) {
        let mut entries: Vec<Entry> = Vec::new();
        let mut bad = 0usize;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let (Some(word), Some(freq), Some(key)) =
                (parts.next(), parts.next().map(|s| s.trim()), parts.next())
            else {
                bad += 1;
                continue;
            };
            let Ok(freq) = freq.parse::<u64>() else {
                bad += 1;
                continue;
            };
            let word = word.trim();
            if !is_hanzi(word)
                || key.is_empty()
                || word.chars().count() != key.split('\'').count()
                || !key.split('\'').all(syllables::is_syllable)
            {
                bad += 1;
                continue;
            }
            if !seen.insert((word.to_string(), key.to_string())) {
                continue;
            }
            entries.push(Entry {
                word: word.to_string(),
                key: key.to_string(),
                freq,
            });
        }
        entries.sort_by(|a, b| {
            a.key.cmp(&b.key)
                .then(b.freq.cmp(&a.freq))
                .then(a.word.cmp(&b.word))
        });
        let sig_index = {
            let mut idx: Vec<(String, usize)> = entries
                .iter()
                .enumerate()
                .map(|(i, e)| (signature(&e.key), i))
                .collect();
            idx.sort();
            idx
        };
        (
            Dictionary {
                entries,
                sig_index,
            },
            bad,
        )
    }

    pub fn from_path(path: &Path) -> std::io::Result<(Dictionary, usize)> {
        let text = std::fs::read_to_string(path)?;
        Ok(Self::from_text(&text))
    }

    /// 内置迷你词库(离线开发与 E2E 用)。
    pub fn builtin() -> Dictionary {
        let (dict, bad) = Self::from_text(include_str!("../../../dicts/mini.txt"));
        assert_eq!(bad, 0, "内置词库不应有坏行");
        dict
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 精确键查询(全拼音节键):二分找 key 的连续区段。
    pub fn lookup_exact(&self, key: &str) -> Vec<&Entry> {
        let lo = self.entries.partition_point(|e| e.key.as_str() < key);
        let mut out = Vec::new();
        for e in &self.entries[lo..] {
            if e.key != key {
                break;
            }
            out.push(e);
        }
        out
    }

    /// 签名查询:返回该签名的连续区段(调用方再逐音节过滤)。
    pub fn lookup_sig(&self, sig: &str) -> Vec<&Entry> {
        let lo = self
            .sig_index
            .partition_point(|(s, _)| s.as_str() < sig);
        let mut out = Vec::new();
        for (s, i) in &self.sig_index[lo..] {
            if s != sig {
                break;
            }
            out.push(&self.entries[*i]);
        }
        out
    }
}
