//! 词表查询:一串音节单元能读成哪些词。
//!
//! 单元已经由 [`crate::channel`] 定好了读音,这里不再展开模糊音或纠错:全拼单元逐字匹配
//! 键,简拼单元匹配音节前缀。音节图的每条路径对应一次查询,靠键前缀剪枝,相当于沿词典
//! 走一棵音节前缀树。

use std::collections::HashMap;
use std::rc::Rc;

use lueying_dict::{Dictionary, Entry, signature};

use crate::channel::Unit;
use crate::user::UserDict;

pub(crate) const CHAR_BONUS: f64 = 0.5;
const USER_W: f64 = 3.0;
/// 每词固定罚:抵消归一化带来的「分词越少越占优」以外的碎片投机。
/// 2.2 在项目、外部手写、白霜脚本和中立难例上同时不退步。
pub(crate) const WORD_PENALTY: f64 = 2.2;
/// 索引扫描的访问上限(保护首键延迟)。
const SCAN_VISIT_CAP: usize = 8000;

/// 一条词命中。`prior` 只含语言分,读法代价由解码器加上。
pub(crate) struct Hit {
    pub word: Rc<str>,
    /// 词条的音节键。
    pub key: Rc<str>,
    pub prior: f64,
    /// 词典命中;用户独有词为假。
    pub dictionary: bool,
    /// 词典词频;用户独有词为 0。
    pub freq: u64,
    /// 同一按键下用户选过该词的次数。
    pub user_freq: u32,
    /// 用户就在打出的这组音节上学过这个词,不是经模糊读法碰上的。
    pub learned: bool,
}

/// 用户词库里的一条匹配。
struct UserMatch {
    freq: u32,
    key: String,
    learned: bool,
}

/// 用户选词次数带来的语言分。重排要单独看到这一份,见 `Candidate::personal`。
pub(crate) fn user_gain(user_freq: u32) -> f64 {
    USER_W * (1.0 + f64::from(user_freq)).ln()
}

fn prior(word: &str, base: f64, user_freq: u32, total: f64) -> f64 {
    let chars = word.chars().count().max(1) as f64;
    (base / total).ln() + CHAR_BONUS * chars + user_gain(user_freq) - WORD_PENALTY
}

/// 纯用户词(词典没有)按使用次数分三档给基准词频:首次出现的自造词只给很低的基准,
/// 先落在候选后段;连续使用后逐级抬高,避免一次误学就把陌生词顶到首选。
fn user_base(user_freq: u32) -> f64 {
    match user_freq {
        0 | 1 => 1_000.0,
        2 => 30_000.0,
        _ => 1_000_000.0,
    }
}

/// 用户键里的简拼单元:单字母,或 zh/ch/sh。
fn is_abbrev_key(e: &str) -> bool {
    e.len() == 1 || matches!(e, "zh" | "ch" | "sh")
}

/// 词条键是否逐音节匹配单元。
fn entry_matches(key: &str, units: &[&Unit]) -> bool {
    let mut parts = key.split('\'');
    units.iter().all(|unit| {
        parts.next().is_some_and(|part| {
            if unit.abbrev {
                part.starts_with(unit.text.as_str())
            } else {
                part == unit.text
            }
        })
    }) && parts.next().is_none()
}

/// 签名区段内的词条按键排序,首单元相同的词条连成一段,不用逐条核对整个区段。
fn within_first_unit<'e, 'a>(entries: &'e [Entry<'a>], units: &[&Unit]) -> &'e [Entry<'a>] {
    let Some(first) = units.first() else {
        return entries;
    };
    let prefix = if first.abbrev {
        first.text.clone()
    } else {
        format!("{}'", first.text)
    };
    let start = entries.partition_point(|entry| entry.key < prefix.as_str());
    let len = entries[start..].partition_point(|entry| entry.key.starts_with(prefix.as_str()));
    &entries[start..start + len]
}

/// 用户键是否匹配单元。学习时可能用简拼、查询时用全拼(或反过来),两边按同一音节识别:
/// `z` 之于 `zhi`、`zh` 之于 `zi` 都是同一音节没写全。`typed` 为真时比用户打出的音节,
/// 否则比这条读法的音节。
fn user_key_matches(key: &str, units: &[&Unit], typed: bool) -> bool {
    let mut parts = key.split('\'');
    units.iter().all(|unit| {
        let text = if typed { &unit.literal } else { &unit.text };
        parts.next().is_some_and(|part| {
            let same_first = part.as_bytes().first() == text.as_bytes().first();
            let same = if unit.abbrev {
                part.starts_with(text.as_str())
            } else {
                part == text
            };
            same || (is_abbrev_key(part) && same_first)
        })
    }) && parts.next().is_none()
}

/// 用户词库的索引形状:音节数与首字母。模糊读法可能改首字母,打出的和读成的都要查。
fn shapes(units: &[&Unit]) -> Vec<(usize, char)> {
    let mut out = Vec::with_capacity(2);
    if let Some(unit) = units.first() {
        out.extend(unit.text.chars().next().map(|c| (units.len(), c)));
        out.extend(unit.literal.chars().next().map(|c| (units.len(), c)));
        out.dedup();
    }
    out
}

fn joined(units: &[&Unit]) -> String {
    units
        .iter()
        .map(|unit| unit.text.as_str())
        .collect::<Vec<_>>()
        .join("'")
}

/// 缓存键。`a`、`e`、`o` 既是音节也是简拼,文本相同但匹配规则不同,要分开。
fn identity(units: &[&Unit]) -> String {
    let mut out = String::new();
    for unit in units {
        out.push_str(&unit.text);
        out.push('/');
        out.push_str(&unit.literal);
        out.push(if unit.abbrev { '*' } else { '\'' });
    }
    out
}

pub(crate) struct Lexicon<'a> {
    dict: &'a Dictionary,
    user: &'a UserDict,
    total: f64,
    hits: HashMap<String, Rc<Vec<Hit>>>,
    /// 同一签名被多串单元共用:模糊读法的声母不变时签名相同。
    signatures: HashMap<String, Rc<Vec<Entry<'a>>>>,
}

impl<'a> Lexicon<'a> {
    pub(crate) fn new(dict: &'a Dictionary, user: &'a UserDict, total: f64) -> Self {
        Self {
            dict,
            user,
            total,
            hits: HashMap::new(),
            signatures: HashMap::new(),
        }
    }

    pub(crate) fn lookup(&mut self, units: &[&Unit]) -> Rc<Vec<Hit>> {
        let id = identity(units);
        if let Some(hits) = self.hits.get(&id) {
            return Rc::clone(hits);
        }
        let users = self.user_matches(units);
        let key = joined(units);
        let dict = self.dict;
        let entries: Vec<Entry<'a>> = if units.iter().any(|unit| unit.abbrev) {
            let scanned = self.scan(&signature(&key));
            within_first_unit(&scanned, units)
                .iter()
                .filter(|entry| entry_matches(entry.key, units))
                .copied()
                .collect()
        } else {
            dict.lookup_exact(&key)
        };
        let mut hits: Vec<Hit> = entries
            .iter()
            .map(|entry| {
                let user = users.get(entry.word);
                let user_freq = user.map_or(0, |found| found.freq);
                Hit {
                    word: Rc::from(entry.word),
                    key: Rc::from(entry.key),
                    prior: prior(entry.word, entry.freq.max(1) as f64, user_freq, self.total),
                    dictionary: true,
                    freq: entry.freq,
                    user_freq,
                    learned: user.is_some_and(|found| found.learned),
                }
            })
            .collect();
        // 用户词并入:词典已有的词已带上提频,只补词典没有的。
        for (word, found) in &users {
            if !hits.iter().any(|hit| &*hit.word == word.as_str()) {
                hits.push(Hit {
                    word: Rc::from(word.as_str()),
                    key: Rc::from(found.key.as_str()),
                    prior: prior(word, user_base(found.freq), found.freq, self.total),
                    dictionary: false,
                    freq: 0,
                    user_freq: found.freq,
                    learned: found.learned,
                });
            }
        }
        // 解码只取前面几条,按语言分排好就不用为其余的词建节点。
        hits.sort_by(|a, b| b.prior.total_cmp(&a.prior));
        let hits = Rc::new(hits);
        self.hits.insert(id, Rc::clone(&hits));
        hits
    }

    fn scan(&mut self, sig: &str) -> Rc<Vec<Entry<'a>>> {
        if let Some(entries) = self.signatures.get(sig) {
            return Rc::clone(entries);
        }
        let dict = self.dict;
        let entries = Rc::new(dict.lookup_sig(sig, SCAN_VISIT_CAP));
        self.signatures.insert(sig.to_string(), Rc::clone(&entries));
        entries
    }

    /// 能匹配这串单元的用户词及最常用的一条键。词典命中也要用这份结果提频:
    /// 同一个词用简拼学过,换成全拼输入时仍然应该靠前。
    fn user_matches(&self, units: &[&Unit]) -> HashMap<String, UserMatch> {
        let mut users: HashMap<String, UserMatch> = HashMap::new();
        if self.user.is_empty() {
            return users;
        }
        self.user
            .for_each_matching(&shapes(units), |key, word, freq| {
                let learned = user_key_matches(key, units, true);
                if !learned && !user_key_matches(key, units, false) {
                    return;
                }
                let found = users.entry(word.to_string()).or_insert_with(|| UserMatch {
                    freq: 0,
                    key: key.to_string(),
                    learned,
                });
                found.learned |= learned;
                if freq > found.freq {
                    found.freq = freq;
                    found.key = key.to_string();
                }
            });
        users
    }

    /// 还能不能接更长的词:用键或签名前缀判断,避免无意义的组合爆炸。
    pub(crate) fn may_extend(&self, units: &[&Unit]) -> bool {
        // 用户词可能不在主词典的前缀索引里。只查主词典会在用户独有词的
        // 首个音节之后剪枝,整串查询永远命中不了(如「日期」的 `ri'qi`)。
        let initials: Vec<char> = shapes(units).into_iter().map(|shape| shape.1).collect();
        if self.user.may_extend(units.len(), &initials) {
            return true;
        }
        let key = joined(units);
        if units.iter().any(|unit| unit.abbrev) {
            return self.dict.has_sig_prefix(&signature(&key));
        }
        self.dict.has_key_prefix(&format!("{key}'"))
    }
}
