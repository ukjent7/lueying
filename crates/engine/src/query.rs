//! 候选生成:对每条切分产出「前缀词候选」与「整句贪心路径」,跨切分合并、去重、排序。
//!
//! 打分:`ln(词频) + 0.5×字数 + 3×ln(1+用户频次)`;整句候选得分为贪心路径各段之和,
//! 多字词自然领先。无匹配音节的切分不出整句候选(该切分作废)。

use std::collections::HashMap;
use std::rc::Rc;

use pinyin_dict::syllables::{self, Fuzzy};
use pinyin_dict::{Dictionary, signature};

use crate::segment::Syll;
use crate::user::UserDict;

/// 展示给上层的候选。
#[derive(Clone, Debug)]
pub struct Cand {
    pub text: String,
    /// 吃掉的音节数(整句候选 = 切分的全部音节)。
    pub consume: usize,
    pub score: f64,
    /// 整句贪心路径(覆盖整条切分)。一元词频模型下单字链频次和天然占优,
    /// 排序须先「整句优先」,再「词数少优先」,才不会出现 嗲你拿哦 压过 电脑。
    pub full: bool,
    /// 路径里的词数(前缀词恒为 1)。
    pub words: usize,
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
/// 每个前缀档位保留的最高分词条数。
const PER_L_CAP: usize = 12;
/// 索引扫描的访问上限(保护首键延迟)。
const SCAN_VISIT_CAP: usize = 8000;

#[derive(Clone)]
struct Hit {
    word: String,
    /// 构造时一次算好(排序/贪心反复取分,不能每次都算三个 ln)。
    score: f64,
}

impl Hit {
    fn new(word: String, freq: u64, user_freq: u32) -> Self {
        let chars = word.chars().count().max(1) as f64;
        let base = if freq == 0 {
            1_000_000.0 // 纯用户词(词典没有)的基准频次
        } else {
            freq as f64
        };
        Self {
            score: base.ln() + CHAR_BONUS * chars + USER_W * (1.0 + f64::from(user_freq)).ln(),
            word,
        }
    }

    fn score(&self) -> f64 {
        self.score
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
    exp_cache: &mut HashMap<String, Rc<Vec<String>>>,
) -> bool {
    let mut parts = entry_key.split('\'');
    for t in sylls {
        let Some(e) = parts.next() else {
            return false;
        };
        let ok = if t.abbrev {
            e.starts_with(t.text.as_str())
        } else if *fuzzy == Fuzzy::default() {
            e == t.text
        } else {
            let exps = exp_cache
                .entry(t.text.clone())
                .or_insert_with(|| Rc::new(syllables::expansions(&t.text, fuzzy)));
            exps.iter().any(|x| x == e)
        };
        if !ok {
            return false;
        }
    }
    parts.next().is_none()
}

struct Querier<'a> {
    dict: &'a Dictionary,
    user: &'a UserDict,
    fuzzy: Fuzzy,
    cache: HashMap<String, Rc<Vec<Hit>>>,
    exp_cache: HashMap<String, Rc<Vec<String>>>,
}

impl Querier<'_> {
    fn lookup(&mut self, sylls: &[Syll]) -> Rc<Vec<Hit>> {
        let key = typed_key(sylls);
        if let Some(hits) = self.cache.get(&key) {
            return Rc::clone(hits);
        }
        let hits = Rc::new(self.compute(sylls, &key));
        self.cache.insert(key, Rc::clone(&hits));
        hits
    }

    fn compute(&mut self, sylls: &[Syll], key: &str) -> Vec<Hit> {
        let any_abbrev = sylls.iter().any(|s| s.abbrev);
        let fuzzy_on = self.fuzzy != Fuzzy::default();
        let mut hits: Vec<Hit> = Vec::new();
        if !any_abbrev && !fuzzy_on {
            for e in self.dict.lookup_exact(key) {
                hits.push(Hit::new(
                    e.word.clone(),
                    e.freq,
                    self.user.freq(key, &e.word),
                ));
            }
        } else if !fuzzy_on && sylls.first().is_some_and(|s| !s.abbrev) {
            // 前导全拼音节已知:按键前缀扫全拼索引,区间比签名细得多(ni'h 只扫 ni* 词)。
            let full_len = sylls.iter().take_while(|s| !s.abbrev).count();
            let mut prefix = typed_key(&sylls[..full_len]);
            prefix.push('\'');
            for e in self.dict.lookup_prefix(&prefix, SCAN_VISIT_CAP) {
                if matches(&e.key, sylls, &self.fuzzy, &mut self.exp_cache) {
                    hits.push(Hit::new(
                        e.word.clone(),
                        e.freq,
                        self.user.freq(key, &e.word),
                    ));
                }
            }
        } else {
            // 纯简拼或模糊音:签名区间(模糊首字母时枚举翻转变体)。
            let base = signature(key);
            for sig in self.sig_variants(&base, sylls) {
                for e in self.dict.lookup_sig(&sig, SCAN_VISIT_CAP) {
                    if matches(&e.key, sylls, &self.fuzzy, &mut self.exp_cache) {
                        hits.push(Hit::new(
                            e.word.clone(),
                            e.freq,
                            self.user.freq(key, &e.word),
                        ));
                    }
                }
            }
        }
        // 用户词并入:词典已有的词已带上提频,只补词典没有的。
        for (ukey, uword, ufreq) in self.user.iter() {
            if matches(ukey, sylls, &self.fuzzy, &mut self.exp_cache)
                && !hits.iter().any(|h| h.word == uword)
            {
                hits.push(Hit::new(uword.to_string(), 0, ufreq));
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
fn greedy_path(querier: &mut Querier, sylls: &[Syll]) -> Option<(String, f64, usize)> {
    let mut text = String::new();
    let mut score = 0.0;
    let mut words = 0usize;
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
        words += 1;
        pos += l;
    }
    (!text.is_empty()).then_some((text, score, words))
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
            // 每档只取分数最高的若干词,先 select_nth 再排序,避免全量排序数千命中。
            let hits = querier.lookup(&seg[..l]);
            let mut top: Vec<&Hit> = hits.iter().collect();
            if top.len() > PER_L_CAP {
                top.select_nth_unstable_by(PER_L_CAP, |a, b| b.score().total_cmp(&a.score()));
                top.truncate(PER_L_CAP);
            }
            top.sort_by(|a, b| b.score().total_cmp(&a.score()));
            for hit in top {
                cands.push(Cand {
                    text: hit.word.clone(),
                    consume: l,
                    score: hit.score(),
                    full: l == seg.len(),
                    words: 1,
                    spans: spans.clone(),
                });
            }
        }
        if let Some((text, score, words)) = greedy_path(&mut querier, seg) {
            cands.push(Cand {
                text,
                consume: seg.len(),
                score,
                full: true,
                words,
                spans,
            });
        }
    }
    // 同 (text, consume) 留整句版本与高分;再按「整句优先、词数少优先、分数、吃音节数」排序。
    cands.sort_by(|a, b| {
        a.text
            .cmp(&b.text)
            .then(a.consume.cmp(&b.consume))
            .then(b.full.cmp(&a.full))
            .then(b.score.total_cmp(&a.score))
    });
    cands.dedup_by(|a, b| a.text == b.text && a.consume == b.consume);
    cands.sort_by(|a, b| {
        b.full
            .cmp(&a.full)
            .then(a.words.cmp(&b.words))
            .then(b.score.total_cmp(&a.score))
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
