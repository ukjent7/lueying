//! 双拼输入方案:两键一音节,展开成规范全拼后交给核心词图。
//!
//! 布局数据取自公开方案表,内置小鹤、自然码、微软、搜狗四套。展开时同时
//! 记录查询串与原始击键的双向位置映射,核心候选的 `spans` 因此仍能按用户
//! 实际敲的键回删与逐段上屏。

use std::collections::HashMap;

use lueying_dict::syllables;
use lueying_ext::{Expansion, Scheme, SchemeManifest};

use crate::SchemeId;

struct Layout {
    id: &'static str,
    name: &'static str,
    initials: &'static [(char, &'static str)],
    finals: &'static [(char, &'static [&'static str])],
    zero: &'static [(char, &'static [&'static str])],
    zero_pairs: &'static [(&'static str, &'static str)],
    extra: &'static [char],
}

static XIAOHE: Layout = Layout {
    id: "xiaohe",
    name: "小鹤双拼",
    initials: &[('v', "zh"), ('i', "ch"), ('u', "sh")],
    finals: &[
        ('a', &["a"]),
        ('o', &["uo", "o"]),
        ('e', &["e"]),
        ('i', &["i"]),
        ('u', &["u"]),
        ('v', &["ui", "v"]),
        ('b', &["in"]),
        ('c', &["ao"]),
        ('d', &["ai"]),
        ('f', &["en"]),
        ('g', &["eng"]),
        ('h', &["ang"]),
        ('j', &["an"]),
        ('k', &["uai", "ing"]),
        ('l', &["iang", "uang"]),
        ('m', &["ian"]),
        ('n', &["iao"]),
        ('p', &["ie"]),
        ('q', &["iu"]),
        ('r', &["uan", "er"]),
        ('s', &["ong", "iong"]),
        ('t', &["ue", "ve"]),
        ('w', &["ei"]),
        ('x', &["ia", "ua"]),
        ('y', &["un"]),
        ('z', &["ou"]),
    ],
    zero: &[
        ('a', &["a", "ai", "an", "ang", "ao"]),
        ('o', &["o", "ou"]),
        ('e', &["e", "ei", "en", "eng", "er"]),
    ],
    zero_pairs: &[],
    extra: &[],
};

static ZIRANMA: Layout = Layout {
    id: "ziranma",
    name: "自然码",
    initials: &[('v', "zh"), ('i', "ch"), ('u', "sh")],
    finals: &[
        ('a', &["a"]),
        ('o', &["uo", "o"]),
        ('e', &["e"]),
        ('i', &["i"]),
        ('u', &["u"]),
        ('v', &["ui", "v"]),
        ('b', &["ou"]),
        ('c', &["iao"]),
        ('d', &["uang", "iang"]),
        ('f', &["en"]),
        ('g', &["eng"]),
        ('h', &["ang"]),
        ('j', &["an"]),
        ('k', &["ao"]),
        ('l', &["ai"]),
        ('m', &["ian"]),
        ('n', &["in"]),
        ('p', &["un"]),
        ('q', &["iu"]),
        ('r', &["uan", "er"]),
        ('s', &["ong", "iong"]),
        ('t', &["ue", "ve"]),
        ('w', &["ia", "ua"]),
        ('x', &["ie"]),
        ('y', &["uai", "ing"]),
        ('z', &["ei"]),
    ],
    zero: &[
        ('a', &["a", "ai", "an", "ang", "ao"]),
        ('o', &["o", "ou"]),
        ('e', &["e", "ei", "en", "eng", "er"]),
    ],
    zero_pairs: &[],
    extra: &[],
};

static MSPY: Layout = Layout {
    id: "mspy",
    name: "微软双拼",
    initials: &[('v', "zh"), ('i', "ch"), ('u', "sh")],
    finals: &[
        ('a', &["a"]),
        ('o', &["uo", "o"]),
        ('e', &["e"]),
        ('i', &["i"]),
        ('u', &["u"]),
        ('v', &["ui"]),
        ('b', &["ou"]),
        ('c', &["iao"]),
        ('d', &["uang", "iang"]),
        ('f', &["en"]),
        ('g', &["eng"]),
        ('h', &["ang"]),
        ('j', &["an"]),
        ('k', &["ao"]),
        ('l', &["ai"]),
        ('m', &["ian"]),
        ('n', &["in"]),
        ('p', &["un"]),
        ('q', &["iu"]),
        ('r', &["uan", "er"]),
        ('s', &["ong", "iong"]),
        ('t', &["ue", "ve"]),
        ('w', &["ia", "ua"]),
        ('x', &["ie"]),
        ('y', &["uai", "v"]),
        ('z', &["ei"]),
        (';', &["ing"]),
    ],
    zero: &[(
        'o',
        &[
            "a", "ai", "an", "ang", "ao", "e", "ei", "en", "eng", "er", "o", "ou",
        ],
    )],
    zero_pairs: &[],
    extra: &[';'],
};

static SOGOU: Layout = Layout {
    id: "sogou",
    name: "搜狗双拼",
    initials: &[('v', "zh"), ('i', "ch"), ('u', "sh")],
    finals: &[
        ('a', &["a"]),
        ('o', &["uo", "o"]),
        ('e', &["e"]),
        ('i', &["i"]),
        ('u', &["u"]),
        ('v', &["ui"]),
        ('b', &["ou"]),
        ('c', &["iao"]),
        ('d', &["uang", "iang"]),
        ('f', &["en"]),
        ('g', &["eng"]),
        ('h', &["ang"]),
        ('j', &["an"]),
        ('k', &["ao"]),
        ('l', &["ai"]),
        ('m', &["ian"]),
        ('n', &["in"]),
        ('p', &["un"]),
        ('q', &["iu"]),
        ('r', &["uan", "er"]),
        ('s', &["ong", "iong"]),
        ('t', &["ue", "ve"]),
        ('w', &["ia", "ua"]),
        ('x', &["ie"]),
        ('y', &["uai", "v"]),
        ('z', &["ei"]),
        (';', &["ing"]),
    ],
    zero: &[(
        'o',
        &[
            "a", "ai", "an", "ang", "ao", "e", "ei", "en", "eng", "er", "o", "ou",
        ],
    )],
    zero_pairs: &[],
    extra: &[';'],
};

const LETTERS: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
    "s", "t", "u", "v", "w", "x", "y", "z",
];

/// 一套可用的双拼方案。
pub struct Shuangpin {
    layout: &'static Layout,
    initials: HashMap<char, &'static str>,
    finals: HashMap<char, &'static [&'static str]>,
    zero: HashMap<char, &'static [&'static str]>,
    zero_pairs: HashMap<[u8; 2], &'static str>,
}

impl Shuangpin {
    /// 按方案 id 构造;未知 id 返回 `None`。
    pub fn by_id(id: &str) -> Option<Self> {
        let layout = match id {
            "xiaohe" => &XIAOHE,
            "ziranma" => &ZIRANMA,
            "mspy" => &MSPY,
            "sogou" => &SOGOU,
            _ => return None,
        };
        let mut initials = HashMap::new();
        for &(key, value) in layout.initials {
            initials.insert(key, value);
        }
        let mut finals = HashMap::new();
        for &(key, value) in layout.finals {
            finals.insert(key, value);
        }
        let mut zero = HashMap::new();
        for &(key, value) in layout.zero {
            zero.insert(key, value);
        }
        let mut zero_pairs = HashMap::new();
        for &(key, value) in layout.zero_pairs {
            let bytes = key.as_bytes();
            zero_pairs.insert([bytes[0], bytes[1]], value);
        }
        Some(Self {
            layout,
            initials,
            finals,
            zero,
            zero_pairs,
        })
    }

    fn initial(&self, key: char) -> Option<&'static str> {
        if let Some(value) = self.initials.get(&key) {
            return Some(*value);
        }
        if key.is_ascii_lowercase() {
            return Some(LETTERS[(key as u8 - b'a') as usize]);
        }
        None
    }

    fn finals_of(&self, key: char) -> &'static [&'static str] {
        self.finals.get(&key).copied().unwrap_or(&[])
    }

    fn zero_of(&self, key: char) -> &'static [&'static str] {
        self.zero.get(&key).copied().unwrap_or(&[])
    }

    fn pair(&self, first: char, second: char) -> Option<String> {
        if let Some(syllable) = self.zero_pairs.get(&[first as u8, second as u8]) {
            return Some((*syllable).to_string());
        }
        let zero = self.zero_of(first);
        if !zero.is_empty() {
            let literal = format!("{first}{second}");
            if zero.contains(&literal.as_str()) && syllables::is_syllable(&literal) {
                return Some(literal);
            }
            let finals = self.finals_of(second);
            for final_part in finals {
                if zero.contains(final_part) && syllables::is_syllable(final_part) {
                    return Some((*final_part).to_string());
                }
            }
            for syllable in zero {
                if finals.contains(&final_of(syllable)) && syllables::is_syllable(syllable) {
                    return Some((*syllable).to_string());
                }
            }
            if first == second {
                let single = first.to_string();
                if zero.contains(&single.as_str()) && syllables::is_syllable(&single) {
                    return Some(single);
                }
            }
        }
        let initial = self.initial(first)?;
        for final_part in self.finals_of(second) {
            let syllable = normalize(&format!("{initial}{final_part}"));
            if syllables::is_syllable(&syllable) {
                return Some(syllable);
            }
        }
        None
    }
}

impl Scheme for Shuangpin {
    fn manifest(&self) -> SchemeManifest {
        SchemeManifest {
            id: self.layout.id,
            name: self.layout.name,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn expand(&self, raw: &str) -> Expansion {
        let mut query = String::with_capacity(raw.len() * 2);
        let mut raw_at = vec![0usize];
        let mut segments = Vec::new();
        let mut key_parts = Vec::new();
        let mut at = 0usize;
        while at < raw.len() {
            if raw.as_bytes()[at] == b'\'' {
                at += 1;
                continue;
            }
            let end = raw[at..]
                .find('\'')
                .map_or(raw.len(), |offset| at + offset);
            let segment = &raw[at..end];
            let mut index = 0usize;
            while index < segment.len() {
                let remain = segment.len() - index;
                let raw_start = at + index;
                let (text, width) = if remain == 1 {
                    let key = segment.as_bytes()[index] as char;
                    (
                        self.initial(key)
                            .map_or_else(|| key.to_string(), ToString::to_string),
                        1,
                    )
                } else {
                    let first = segment.as_bytes()[index] as char;
                    let second = segment.as_bytes()[index + 1] as char;
                    (
                        self.pair(first, second)
                            .unwrap_or_else(|| format!("{first}{second}")),
                        2,
                    )
                };
                // 双拼的两键天然是音节边界。查询串保留隔音符,核心音节图就不会
                // 把相邻音节重新切错(如 xian / xi'an);raw_at 同步多一个边界。
                if !key_parts.is_empty() {
                    query.push('\'');
                    raw_at.push(raw_start);
                }
                append(&mut query, &mut raw_at, &text, raw_start, raw_start + width);
                key_parts.push(syllables::canonical(&text).to_string());
                segments.push((raw_start, raw_start + width));
                index += width;
            }
            at = end;
        }
        let raw_to_query = invert(&raw_at, raw.len());
        Expansion {
            query,
            raw_at,
            raw_to_query,
            key: key_parts.join("'"),
            segments,
            filter: None,
            aux_start: None,
        }
    }

    fn segments(&self, raw: &str) -> Vec<(usize, usize)> {
        self.expand(raw).segments
    }

    fn accepts_char(&self, c: char) -> bool {
        self.layout.extra.contains(&c)
    }

    fn key_for_spans(&self, raw: &str, spans: &[(usize, usize)]) -> String {
        let expansion = self.expand(raw);
        let mut parts = Vec::new();
        for &(start, end) in spans {
            let query_start = expansion.raw_to_query.get(start).copied().unwrap_or(0);
            let query_end = expansion
                .raw_to_query
                .get(end)
                .copied()
                .unwrap_or(expansion.query.len());
            for part in expansion.query[query_start..query_end].split('\'') {
                if !part.is_empty() {
                    parts.push(syllables::canonical(part).to_string());
                }
            }
        }
        parts.join("'")
    }
}

fn append(query: &mut String, raw_at: &mut Vec<usize>, text: &str, start: usize, end: usize) {
    let length = text.len();
    if length == 0 {
        return;
    }
    query.push_str(text);
    let raw_length = end - start;
    for index in 1..=length {
        raw_at.push(start + index * raw_length / length);
    }
}

fn invert(raw_at: &[usize], raw_len: usize) -> Vec<usize> {
    let mut out = vec![0usize; raw_len + 1];
    let mut query = 0usize;
    for (raw, slot) in out.iter_mut().enumerate() {
        while query < raw_at.len() && raw_at[query] < raw {
            query += 1;
        }
        *slot = query.min(raw_at.len().saturating_sub(1));
    }
    out
}

fn final_of(syllable: &str) -> &str {
    for initial in ["zh", "ch", "sh"] {
        if let Some(rest) = syllable.strip_prefix(initial) {
            return rest;
        }
    }
    if syllable
        .chars()
        .next()
        .is_some_and(|c| "bpmfdtnlgkhjqxrzcsyw".contains(c))
    {
        &syllable[1..]
    } else {
        syllable
    }
}

fn normalize(syllable: &str) -> String {
    if syllable
        .chars()
        .next()
        .is_some_and(|c| matches!(c, 'j' | 'q' | 'x' | 'y'))
    {
        let mut out = syllable.replacen("ve", "ue", 1);
        out = out.replacen("vn", "un", 1);
        out = out.replacen('v', "u", 1);
        out
    } else {
        syllable.to_string()
    }
}

/// 按枚举构造方案。
pub fn build(id: SchemeId) -> Option<Shuangpin> {
    Shuangpin::by_id(id.as_str())
}
