//! 用户词典:选词提频与自动造词,带指数衰减,支持导出/导入。

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// 超过该间隔再选同一词,频次先减半再加一(近似近期使用优先)。
const DECAY_SECS: u64 = 7 * 24 * 3600;

#[derive(Clone)]
struct UserEntry {
    word: String,
    freq: u32,
    ts: u64,
}

#[derive(Default)]
pub struct UserDict {
    map: HashMap<String, Vec<UserEntry>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl UserDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次「在 key 下选了 word」。
    pub fn learn(&mut self, key: &str, word: &str) {
        let ts = now();
        let entries = self.map.entry(key.to_string()).or_default();
        match entries.iter_mut().find(|e| e.word == word) {
            Some(e) => {
                e.freq = if ts.saturating_sub(e.ts) > DECAY_SECS {
                    e.freq / 2 + 1
                } else {
                    e.freq + 1
                };
                e.ts = ts;
            }
            None => entries.push(UserEntry {
                word: word.to_string(),
                freq: 1,
                ts,
            }),
        }
    }

    /// 该键下某词的用户频次(未学过为 0)。
    pub fn freq(&self, key: &str, word: &str) -> u32 {
        self.map
            .get(key)
            .and_then(|v| v.iter().find(|e| e.word == word))
            .map_or(0, |e| e.freq)
    }

    /// 键下的全部用户词条。
    pub fn entries(&self, key: &str) -> impl Iterator<Item = (&str, u32)> {
        self.map
            .get(key)
            .into_iter()
            .flat_map(|v| v.iter().map(|e| (e.word.as_str(), e.freq)))
    }

    /// 全部 (键, 词, 频次),签名扫描用。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, u32)> {
        self.map
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |e| (k.as_str(), e.word.as_str(), e.freq)))
    }

    /// 导出为行文本(`键<TAB>词<TAB>频次<TAB>时间戳`)。
    pub fn export(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.map {
            for e in v {
                out.push_str(&format!("{k}\t{}\t{}\t{}\n", e.word, e.freq, e.ts));
            }
        }
        out
    }

    /// 从 [`Self::export`] 的输出导入,坏行跳过。
    pub fn import(&mut self, text: &str) {
        for line in text.lines() {
            let mut parts = line.split('\t');
            if let (Some(k), Some(w), Some(f), Some(t)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
                && let (Ok(freq), Ok(ts)) = (f.parse::<u32>(), t.parse::<u64>())
                && !k.is_empty()
                && !w.is_empty()
            {
                self.map.entry(k.to_string()).or_default().push(UserEntry {
                    word: w.to_string(),
                    freq,
                    ts,
                });
            }
        }
    }
}
