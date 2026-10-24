//! 候选生成:对每条切分产出「前缀词候选」与「整句贪心路径」,跨切分合并、去重、排序。
//!
//! 打分:`ln(词频) + 0.5×字数 + 3×ln(1+用户频次)`;整句候选得分为贪心路径各段之和,
//! 多字词自然领先。无匹配音节的切分不出整句候选(该切分作废)。

use std::collections::HashMap;

use pinyin_dict::syllables::{self, Fuzzy};
use pinyin_dict::{signature, Dictionary};

use crate::segment::Syll;
use crate::user::UserDict;

/// 展示给上层的候选。
#[derive(Clone, Debug)]
pub struct Cand {
    pub text: String,
    /// 吃掉的音节数(整句候选 = 切分的全部音节)。
    pub consume: usize,
    pub score: f64,
    /// 所属切分各音节的原始区间(回删与逐段上屏用)。
    pub spans: Vec<(usize, usize)>,
}

/// 一次构建的产物。
pub struct Built {
    pub cands: Vec<Cand>,
    /// 内联显示文本:首选候选文本,无候选时原文。
    pub display: String,
}

const CHAR_BONUS: f64 = 0.5;
const USER_W: f64 = 3.0;
const MAX_CANDS: usize = 40;
/// 简拼签名扫描的单次命中上限(保护首键延迟)。
const SIG_SCAN_CAP: usize = 4000;

#[derive(Clone)]
struct Hit {
    word: String,
    freq: u64,
    user_freq: u32,
    /// 仅用户词(词典没有此词)。
    user_only: bool,
}

impl Hit {
    fn score(&self) -> f64 {
        let chars = self.word.chars().count().max(1) as f64;
        let base = if self.user_only {
            1_000_000.0
        } else {
            self.freq as f64
        };
        base.ln() + CHAR_BONUS * chars + USER_W * (1.0 + f64::from(self.user_freq)).ln()
    }
}

fn typed_key(sylls: &[Syll]) -> String {
    sylls
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("'")
}

/// 词条键是否逐音节匹配输入:简拼前缀匹配;全拼等值(或模糊音等价)。
fn matches(
    entry_key: &str,
    sylls: &[Syll],
    fuzzy: &Fuzzy,
    exp_cache: &mut HashMap<String, Vec<String>>,
) -> bool {
    let parts: Vec<&str> = entry_key.split('\'').collect();
    if parts.len() != sylls.len() {
        return false;
    }
    sylls.iter().zip(parts).all(|(t, e)| {
        if t.abbrev {
            e.starts_with(t.text.as_str())
        } else if *fuzzy == Fuzzy::default() {
            e == t.text
        } else {
            let exps = exp_cache
                .entry(t.text.clone())
                .or_insert_with(|| syllables::expansions(&t.text, fuzzy));
            exps.iter().any(|x| x == e)
        }
    })
}

struct Querier<'a> {
    dict: &'a Dictionary,
    user: &'a UserDict,
    fuzzy: Fuzzy,
    cache: HashMap<String, Vec<Hit>>,
    exp_cache: HashMap<String, Vec<String>>,
}

impl Querier<'_> {
    fn lookup(&mut self, sylls: &[Syll]) -> Vec<Hit> {
        let key = typed_key(sylls);
        if let Some(hits) = self.cache.get(&key) {
            return hits.clone();
        }
        let hits = self.compute(sylls, &key);
        self.cache.insert(key, hits.clone());
        hits
    }

    fn compute(&mut self, sylls: &[Syll], key: &str) -> Vec<Hit> {
        let any_abbrev = sylls.iter().any(|s| s.abbrev);
        let fuzzy_on = self.fuzzy != Fuzzy::default();
        let mut hits: Vec<Hit> = Vec::new();
        if !any_abbrev && !fuzzy_on {
            for e in self.dict.lookup_exact(key) {
                hits.push(Hit {
                    word: e.word.clone(),
                    freq: e.freq,
                    user_freq: self.user.freq(key, &e.word),
                    user_only: false,
                });
            }
        } else {
            let base = signature(key);
            for sig in self.sig_variants(&base, sylls) {
                for e in self.dict.lookup_sig(&sig) {
                    if hits.len() >= SIG_SCAN_CAP {
                        break;
                    }
                    if matches(&e.key, sylls, &self.fuzzy, &mut self.exp_cache) {
                        hits.push(Hit {
                            word: e.word.clone(),
                            freq: e.freq,
                            user_freq: self.user.freq(key, &e.word),
                            user_only: false,
                        });
                    }
                }
            }
        }
        // 用户词并入:词典已有的词已带上提频,只补词典没有的。
        for (ukey, uword, ufreq) in self.user.iter() {
            if matches(ukey, sylls, &self.fuzzy, &mut self.exp_cache)
                && !hits.iter().any(|h| h.word == uword)
            {
                hits.push(Hit {
                    word: uword.to_string(),
                    freq: 0,
                    user_freq: ufreq,
                    user_only: true,
                });
            }
        }
        hits
    }

    /// l/n、f/h 模糊会改变签名首字母:枚举前三个翻转位的组合(最多 8 个签名)。
    fn sig_variants(&self, base: &str, sylls: &[Syll]) -> Vec<String> {
        let flips: Vec<usize> = sylls
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                !s.abbrev
                    && ((self.fuzzy.ln && (s.text.starts_with('n') || s.text.starts_with('l')))
                        || (self.fuzzy.fh && (s.text.starts_with('f') || s.text.starts_with('h'))))
            })
            .map(|(i, _)| i)
            .take(3)
            .collect();
        if flips.is_empty() {
            return vec![base.to_string()];
        }
        let mut out = Vec::new();
        for mask in 0..(1u32 << flips.len()) {
            let mut sig: Vec<char> = base.chars().collect();
            for (bit, &pos) in flips.iter().enumerate() {
                if mask & (1 << bit) != 0
                    && let Some(c) = sylls[pos].text.chars().next()
                {
                    sig[pos] = match c {
                        'n' => 'l',
                        'l' => 'n',
                        'f' => 'h',
                        'h' => 'f',
                        _ => c,
                    };
                }
            }
            out.push(sig.into_iter().collect());
        }
        out
    }
}

/// 整句贪心:从左到右取「最长且词典有词」的前缀,直到吃完整串;任一位置无词则作废。
fn greedy_path(querier: &mut Querier, sylls: &[Syll]) -> Option<(String, f64)> {
    let mut text = String::new();
    let mut score = 0.0;
    let mut pos = 0;
    while pos < sylls.len() {
        let mut chosen: Option<(usize, Hit)> = None;
        for l in (1..=sylls.len() - pos).rev() {
            let hits = querier.lookup(&sylls[pos..pos + l]);
            if let Some(best) = hits.iter().max_by(|a, b| a.score().total_cmp(&b.score())) {
                chosen = Some((l, best.clone()));
                break;
            }
        }
        let (l, hit) = chosen?;
        text.push_str(&hit.word);
        score += hit.score();
        pos += l;
    }
    (!text.is_empty()).then_some((text, score))
}

pub fn build(
    dict: &Dictionary,
    user: &UserDict,
    fuzzy: &Fuzzy,
    raw: &str,
    segs: &[Vec<Syll>],
) -> Built {
    let mut querier = Querier {
        dict,
        user,
        fuzzy: *fuzzy,
        cache: HashMap::new(),
        exp_cache: HashMap::new(),
    };
    let mut cands: Vec<Cand> = Vec::new();
    for seg in segs {
        let spans: Vec<(usize, usize)> = seg.iter().map(|s| s.span).collect();
        for l in 1..=seg.len() {
            for hit in querier.lookup(&seg[..l]) {
                cands.push(Cand {
                    text: hit.word.clone(),
                    consume: l,
                    score: hit.score(),
                    spans: spans.clone(),
                });
            }
        }
        if let Some((text, score)) = greedy_path(&mut querier, seg) {
            cands.push(Cand {
                text,
                consume: seg.len(),
                score,
                spans,
            });
        }
    }
    // 同 (text, consume) 留最大分,再按分数、吃音节数、文本排序,保证确定性。
    cands.sort_by(|a, b| {
        a.text
            .cmp(&b.text)
            .then(a.consume.cmp(&b.consume))
            .then(b.score.total_cmp(&a.score))
    });
    cands.dedup_by(|a, b| a.text == b.text && a.consume == b.consume);
    cands.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.consume.cmp(&a.consume))
            .then(a.text.cmp(&b.text))
    });
    cands.truncate(MAX_CANDS);
    let display = cands
        .first()
        .map(|c| c.text.clone())
        .unwrap_or_else(|| raw.to_string());
    Built { cands, display }
}
