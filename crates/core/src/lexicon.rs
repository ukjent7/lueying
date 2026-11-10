//! 词表查询:一串音节单元能读成哪些词。
//!
//! 单元已经由 [`crate::channel`] 定好了读音,这里不再展开模糊音或纠错:全拼单元逐字匹配
//! 键,简拼单元匹配音节前缀。音节图的每条路径对应一次查询,靠键前缀剪枝,相当于沿词典
//! 走一棵音节前缀树。

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use lueying_dict::syllables::is_syllable;
use lueying_dict::{Dictionary, Entry, signature};

use crate::channel::Unit;
use crate::query::Candidate;
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
    pub word: Arc<str>,
    /// 词条的音节键。
    pub key: Arc<str>,
    pub prior: f64,
    /// 词典命中;用户独有词为假。
    pub dictionary: bool,
    /// 词典词频;用户独有词为 0。
    pub freq: u64,
    /// 同一按键下用户对该词的有效层级(见 [`crate::user::Memory::level`])。
    pub level: f64,
    /// 用户就在打出的这组音节上学过这个词,不是经模糊读法碰上的。
    pub learned: bool,
}

/// 一个音节单元能否给学习加分。
/// 手误纠正或模糊改动过的单元一律不合格,不管它是不是完整音节(`hne` 纠成 `hen`、`diqn` 都是)。
/// 精确、未编辑的读音合格。简拼单元(只用声母或首字母匹配、未做编辑)在所敲字母不是完整音节时合格,
/// 例如 `wm`、`zhd`:这时所有候选都走同一个信道,学习加分只在它们之间排序,不会盖过精确读音。
/// 本身是完整音节的简拼(如 `a`、`ke`、`wei`)不合格;末音节补全(`a` → `ao`)同样不合格。
fn unit_eligible(unit: &Unit) -> bool {
    !unit.typo && unit.edits == 0 && (!unit.abbrev || !is_syllable(&unit.literal))
}

/// 一条路径的每个单元都能给学习加分时,整条路径才可以(词级与词序共用此判断)。
pub(crate) fn bonus_eligible(units: &[&Unit]) -> bool {
    units.iter().all(|unit| unit_eligible(unit))
}

/// 用户词库里的一条匹配。
struct UserMatch {
    level: f64,
    uses: u32,
    key: String,
    /// 只靠简拼与全拼互认命中(输入是完整音节、键是简拼):词典没有确认这组音节的读音时,
    /// 不能凭互认单独成词。
    mutual: bool,
}

/// 用户学习带来的语言分。重排要单独看到这一份,见 `Candidate::personal`。
pub(crate) fn user_gain(level: f64) -> f64 {
    USER_W * (1.0 + level).ln()
}

fn prior(word: &str, base: f64, level: f64, total: f64) -> f64 {
    let chars = word.chars().count().max(1) as f64;
    (base / total).ln() + CHAR_BONUS * chars + user_gain(level) - WORD_PENALTY
}

/// 纯用户词(词典没有词频)按词典最低词频计;学习带来的分数全部来自 [`user_gain`]。
/// 不给固定的高基准:否则闲置后保持率下降的用户词,仅凭基准分就留在候选前列。
const USER_ONLY_FREQ: f64 = 1.0;

/// 原样输入记忆给出的整串候选:击键串上屏过的词,读音路径没有它时按用户词先验入列。
pub(crate) fn raw_candidate(raw_len: usize, word: &str, level: f64, total: f64) -> Candidate {
    let mut cand = Candidate::whole(raw_len, word, prior(word, USER_ONLY_FREQ, level, total));
    cand.personal = user_gain(level);
    cand.level = level;
    cand.learned = true;
    cand
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

/// 用户键是否匹配单元(读音)。只有合格路径才会比较:此时用户打出的音节就是读音。
/// 学习时可能用简拼、查询时用全拼(或反过来),两边按同一音节识别:
/// `z` 之于 `zhi`、`zh` 之于 `zi` 都是同一音节没写全。
fn user_key_matches(key: &str, units: &[&Unit]) -> bool {
    let mut parts = key.split('\'');
    units.iter().all(|unit| {
        parts.next().is_some_and(|part| {
            let text = unit.text.as_str();
            let same_first = part.as_bytes().first() == text.as_bytes().first();
            let same = if unit.abbrev {
                part.starts_with(text)
            } else {
                part == text
            };
            same || (is_abbrev_key(part) && same_first)
        })
    }) && parts.next().is_none()
}

/// 单元的首字母串,与 `UserDict` 的首字母变体对齐:简拼单元取它本身,全拼单元取首字母。
/// `typed` 为真时用用户打出的音节(literal),否则用读音(text)。
fn initials_of(units: &[&Unit], typed: bool) -> String {
    units
        .iter()
        .map(|unit| {
            let text = if typed {
                unit.literal.as_str()
            } else {
                unit.text.as_str()
            };
            if unit.abbrev {
                text.to_string()
            } else {
                text.get(..1).unwrap_or(text).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("'")
}

fn joined(units: &[&Unit]) -> String {
    units
        .iter()
        .map(|unit| unit.text.as_str())
        .collect::<Vec<_>>()
        .join("'")
}

/// 缓存键。`a`、`e`、`o` 既是音节也是简拼,文本相同但匹配规则不同,要分开;
/// 学习资格(`eligible`,见 [`Lexicon::lookup`])不同的查询结果也不同,同样要分开:
/// 模糊变体与精确单元的读音、字面可能完全相同,只有资格不同。
fn identity(units: &[&Unit], eligible: bool) -> String {
    let mut out = String::new();
    out.push(if eligible { '+' } else { '-' });
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
    /// 单元串 → 命中。跨按键保留,由调用方在上文或用户词库修订变化时清空。
    hits: HashMap<String, Arc<Vec<Hit>>>,
    /// 同一签名被多串单元共用:模糊读法的声母不变时签名相同。
    signatures: HashMap<String, Rc<Vec<Entry<'a>>>>,
}

impl<'a> Lexicon<'a> {
    pub(crate) fn new(
        dict: &'a Dictionary,
        user: &'a UserDict,
        total: f64,
        hits: HashMap<String, Arc<Vec<Hit>>>,
    ) -> Self {
        Self {
            dict,
            user,
            total,
            hits,
            signatures: HashMap::new(),
        }
    }

    /// 交还命中缓存,供下一次按键复用。
    pub(crate) fn into_hits(self) -> HashMap<String, Arc<Vec<Hit>>> {
        self.hits
    }

    /// `bonus` 为假时(简拼切进了完整音节),或单元不合格时,这串单元的用户匹配一律不给学习加分。
    pub(crate) fn lookup(&mut self, units: &[&Unit], bonus: bool) -> Arc<Vec<Hit>> {
        let eligible = bonus && bonus_eligible(units);
        let id = identity(units, eligible);
        if let Some(hits) = self.hits.get(&id) {
            return Arc::clone(hits);
        }
        let users = self.user_matches(units, eligible);
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
                let level = user.map_or(0.0, |found| found.level);
                Hit {
                    word: Arc::from(entry.word),
                    key: Arc::from(entry.key),
                    prior: prior(entry.word, entry.freq.max(1) as f64, level, self.total),
                    dictionary: true,
                    freq: entry.freq,
                    level,
                    learned: user.is_some(),
                }
            })
            .collect();
        // 用户词并入:词典已有的词已带上提频,只补词典没有的。
        for (word, found) in &users {
            // 只用过一次的纯用户词还没有证据:不加入候选,否则它会以基准分盖过切分出的整句。
            if found.uses <= 1 {
                continue;
            }
            // 词典里没有这个词,而且它只靠简拼互认命中:不能凭互认新加进候选。
            if found.mutual {
                continue;
            }
            if !hits.iter().any(|hit| &*hit.word == word.as_str()) {
                hits.push(Hit {
                    word: Arc::from(word.as_str()),
                    key: Arc::from(found.key.as_str()),
                    prior: prior(word, USER_ONLY_FREQ, found.level, self.total),
                    dictionary: false,
                    freq: 0,
                    level: found.level,
                    learned: true,
                });
            }
        }
        // 解码只取前面几条,按语言分排好就不用为其余的词建节点。
        hits.sort_by(|a, b| b.prior.total_cmp(&a.prior));
        let hits = Arc::new(hits);
        self.hits.insert(id, Arc::clone(&hits));
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
    fn user_matches(&self, units: &[&Unit], eligible: bool) -> BTreeMap<String, UserMatch> {
        // 按词排序:同分的用户词按固定顺序进候选,与哈希表的迭代顺序无关。
        let mut users: BTreeMap<String, UserMatch> = BTreeMap::new();
        // 只有合格路径(见 [`bonus_eligible`])才给学习加分;其余路径不建用户候选、不改分数,
        // 手误与模糊碰上的词由原样输入记忆负责。
        if self.user.is_empty() || !eligible {
            return users;
        }
        let abbreviated = units.iter().any(|unit| unit.abbrev);
        // 纯简拼段只有一个字母时太模糊:不吃全拼学过的词的跨形式加分(`d` 不因学过「点」而加分)。
        // 同形式的简拼键(`t` 学过的「他」)不受影响;两个及以上的简拼单元照常。
        let lone_letter = units.len() < 2 && units.iter().all(|unit| unit.abbrev);
        let now = crate::user::now();
        for key in self.user.matching_keys(&[initials_of(units, false)]) {
            if lone_letter && key.split('\'').all(|part| !is_abbrev_key(part)) {
                continue;
            }
            for entry in self.user.entries(key) {
                if !user_key_matches(key, units) {
                    continue;
                }
                let level = entry.memory.level(now);
                // 完整音节输入对简拼键:只有词典确认读音的词才能用这条互认。
                let mutual = !abbreviated && key != joined(units);
                let found = users
                    .entry(entry.word.clone())
                    .or_insert_with(|| UserMatch {
                        level: 0.0,
                        uses: entry.memory.uses,
                        key: key.to_string(),
                        mutual: true,
                    });
                found.uses = found.uses.max(entry.memory.uses);
                found.mutual &= mutual;
                if level > found.level {
                    found.level = level;
                    found.key = key.to_string();
                }
            }
        }
        users
    }

    /// 还能不能接更长的词:用键或签名前缀判断,避免无意义的组合爆炸。
    pub(crate) fn may_extend(&self, units: &[&Unit]) -> bool {
        // 用户词可能不在主词典的前缀索引里。只查主词典会在用户独有词的
        // 首个音节之后剪枝,整串查询永远命中不了(如「日期」的 `ri'qi`)。
        let lookups = [initials_of(units, true), initials_of(units, false)];
        if self.user.may_extend(&lookups) {
            return true;
        }
        let key = joined(units);
        if units.iter().any(|unit| unit.abbrev) {
            return self.dict.has_sig_prefix(&signature(&key));
        }
        self.dict.has_key_prefix(&format!("{key}'"))
    }
}
