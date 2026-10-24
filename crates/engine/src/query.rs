//! 候选生成:原始输入上的音节图 + 词图 Viterbi 解码。
//!
//! 词图按**原始字节位置**建:同一串输入的所有切分天然并存,不再枚举切分路径,
//! 也不会因为路径数上限漏掉正确切分。整句分 = Σ 词节点分 + Σ 搭配分。
//!
//! 词节点分把词频归一成对数概率(`ln(freq/total)`),再减每词固定罚:拆成更多
//! 高频碎片的路径要额外付出每词一份的归一化代价。搭配分来自 rime `.gram`
//! 模型(可缺省),只加正分:没有搭配记录不惩罚,避免把「没见过的正确词组」压死。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use pinyin_dict::gram::Gram;
use pinyin_dict::syllables::{self, Fuzzy};
use pinyin_dict::{Dictionary, signature};

use crate::segment::{self, Syll};
use crate::user::UserDict;

/// 展示给上层的候选。
#[derive(Clone, Debug)]
pub struct Cand {
    pub text: String,
    /// 吃掉的音节数。
    pub consume: usize,
    pub score: f64,
    /// 整句候选(覆盖全部输入)。
    pub full: bool,
    /// 路径里的词数(单词候选恒为 1)。
    pub words: usize,
    /// 各音节的原始字节区间(回删与逐段上屏用)。
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
/// 每个 (起点, 终点) 保留的最高分节点数(Viterbi 的束宽)。
const PER_NODE_CAP: usize = 12;
/// 索引扫描的访问上限(保护首键延迟)。
const SCAN_VISIT_CAP: usize = 8000;
/// 一个词最多几个音节(与词库最长词组一致)。
const MAX_WORD_SYL: usize = 8;
/// 每词固定罚:抵消归一化带来的「分词越少越占优」以外的碎片投机。
const WORD_PENALTY: f64 = 3.0;
/// 搭配分权重与零点。取值依据 E2E 整句语料扫描(见 dicts/eval.txt)。
const GRAM_WEIGHT: f64 = 0.3;
const GRAM_BASELINE: f64 = 8.0;
const GRAM_MAX_BONUS: f64 = 4.0;
/// 整句 Viterbi 每档保留几条路径。
const BEAM: usize = 3;
/// 建图访问预算:简拼混合输入的组合可能很多,超出后停止扩词。
const NODE_BUDGET: usize = 4096;

#[derive(Clone)]
struct Hit {
    word: String,
    /// 构造时算好(排序/回溯反复取分,不能每次都算 ln)。
    score: f64,
}

impl Hit {
    /// 词典词条。白霜有大量权重 0 的长尾词,按频次 1 计,天然沉底。
    fn dict(word: String, freq: u64, user_freq: u32, total: f64) -> Self {
        Self::raw(word, freq.max(1) as f64, user_freq, total)
    }

    /// 纯用户词(词典没有):以百万级词频为基准,学习过的词迅速领先。
    fn user_only(word: String, user_freq: u32, total: f64) -> Self {
        Self::raw(word, 1_000_000.0, user_freq, total)
    }

    fn raw(word: String, base: f64, user_freq: u32, total: f64) -> Self {
        let chars = word.chars().count().max(1) as f64;
        Self {
            score: (base / total).ln()
                + CHAR_BONUS * chars
                + USER_W * (1.0 + f64::from(user_freq)).ln()
                - WORD_PENALTY,
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
    total: f64,
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
                hits.push(Hit::dict(
                    e.word.to_string(),
                    e.freq,
                    self.user.freq(key, e.word),
                    self.total,
                ));
            }
        } else if !fuzzy_on && sylls.first().is_some_and(|s| !s.abbrev) {
            // 前导全拼音节已知:按键前缀扫全拼索引,区间比签名细得多(ni'h 只扫 ni* 词)。
            let full_len = sylls.iter().take_while(|s| !s.abbrev).count();
            let mut prefix = typed_key(&sylls[..full_len]);
            prefix.push('\'');
            for e in self.dict.lookup_prefix(&prefix, SCAN_VISIT_CAP) {
                if matches(e.key, sylls, &self.fuzzy, &mut self.exp_cache) {
                    hits.push(Hit::dict(
                        e.word.to_string(),
                        e.freq,
                        self.user.freq(key, e.word),
                        self.total,
                    ));
                }
            }
        } else {
            // 纯简拼或模糊音:签名区间(模糊首字母时枚举翻转变体)。
            let base = signature(key);
            for sig in self.sig_variants(&base, sylls) {
                for e in self.dict.lookup_sig(&sig, SCAN_VISIT_CAP) {
                    if matches(e.key, sylls, &self.fuzzy, &mut self.exp_cache) {
                        hits.push(Hit::dict(
                            e.word.to_string(),
                            e.freq,
                            self.user.freq(key, e.word),
                            self.total,
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
                hits.push(Hit::user_only(uword.to_string(), ufreq, self.total));
            }
        }
        hits
    }

    /// 还能不能接更长的词:用键/签名前缀判断,避免无意义的组合爆炸。
    fn may_extend(&mut self, sylls: &[Syll]) -> bool {
        let key = typed_key(sylls);
        if self.fuzzy != Fuzzy::default() {
            // 模糊音的签名会翻转,前缀判定不成立;交给深度上限与访问预算兜底。
            return true;
        }
        if sylls.iter().any(|s| s.abbrev) {
            self.dict.has_sig_prefix(&signature(&key))
        } else {
            let mut prefix = key;
            prefix.push('\'');
            self.dict.has_key_prefix(&prefix)
        }
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

/// 词图节点:一个词典词覆盖 `[start, end)` 这段原始输入。
#[derive(Clone)]
struct Node {
    start: usize,
    word: String,
    score: f64,
    spans: Vec<(usize, usize)>,
}

/// 搭配分:前一个词到当前词的 `.gram` 加分。查询结果按 (词,词) 缓存。
struct Grammar<'a> {
    gram: Option<&'a Gram>,
    cache: HashMap<(String, String), f64>,
}

impl Grammar<'_> {
    fn bonus(&mut self, prev: &str, word: &str) -> f64 {
        let Some(gram) = self.gram else {
            return 0.0;
        };
        if prev.is_empty() {
            return 0.0;
        }
        let key = (prev.to_string(), word.to_string());
        if let Some(v) = self.cache.get(&key) {
            return *v;
        }
        let v = gram
            .query(prev, word, 3)
            .map_or(0.0, |lnf| (GRAM_WEIGHT * (lnf - GRAM_BASELINE)).clamp(0.0, GRAM_MAX_BONUS));
        self.cache.insert(key, v);
        v
    }
}

/// 从 `pos` 起深度优先枚举词,写到 `by_end[词尾]`。
fn collect(
    q: &mut Querier,
    edges: &[Vec<Syll>],
    raw: &str,
    root: usize,
    pos: usize,
    units: &mut Vec<Syll>,
    spans: &mut Vec<(usize, usize)>,
    by_end: &mut [Vec<Node>],
    visits: &mut usize,
) {
    if !units.is_empty() {
        let hits = q.lookup(units);
        for h in hits.iter() {
            by_end[pos].push(Node {
                start: root,
                word: h.word.clone(),
                score: h.score(),
                spans: spans.clone(),
            });
        }
        *visits += 1;
        if units.len() >= MAX_WORD_SYL || *visits > NODE_BUDGET || !q.may_extend(units) {
            return;
        }
    }
    if pos >= raw.len() {
        return;
    }
    if raw.as_bytes()[pos] == b'\'' {
        // 隔音符是硬边界:词可以跨过它,但不产生音节。
        collect(q, edges, raw, root, pos + 1, units, spans, by_end, visits);
        return;
    }
    for e in &edges[pos] {
        units.push(e.clone());
        spans.push(e.span);
        collect(q, edges, raw, root, e.span.1, units, spans, by_end, visits);
        spans.pop();
        units.pop();
    }
}

/// 每个终点上按 (起点) 分组,去重后保留每组最高分的若干节点。
fn cap_nodes(list: &mut Vec<Node>) {
    list.sort_by(|a, b| a.start.cmp(&b.start).then(b.score.total_cmp(&a.score)));
    let mut out: Vec<Node> = Vec::with_capacity(list.len());
    let mut i = 0;
    while i < list.len() {
        let start = list[i].start;
        let mut seen: Vec<&str> = Vec::new();
        let mut n = 0;
        while i < list.len() && list[i].start == start && n < PER_NODE_CAP {
            if !seen.contains(&list[i].word.as_str()) {
                seen.push(list[i].word.as_str());
                out.push(list[i].clone());
                n += 1;
            }
            i += 1;
        }
        // 越过本组剩余节点。
        while i < list.len() && list[i].start == start {
            i += 1;
        }
    }
    *list = out;
}

#[derive(Clone)]
struct State {
    score: f64,
    path: Vec<(String, Vec<(usize, usize)>)>,
}

pub fn build(dict: &Dictionary, user: &UserDict, fuzzy: &Fuzzy, gram: Option<&Gram>, raw: &str) -> Built {
    let total = dict.total_weight() as f64;
    let mut q = Querier {
        dict,
        user,
        fuzzy: *fuzzy,
        total,
        cache: HashMap::new(),
        exp_cache: HashMap::new(),
    };
    let edges = segment::edges(raw);
    let mut by_end: Vec<Vec<Node>> = vec![Vec::new(); raw.len() + 1];
    let mut visits = 0usize;
    for i in 0..raw.len() {
        if raw.as_bytes()[i] == b'\'' {
            continue;
        }
        collect(
            &mut q,
            &edges,
            raw,
            i,
            i,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut by_end,
            &mut visits,
        );
    }
    for list in &mut by_end {
        cap_nodes(list);
    }

    let mut gram = Grammar { gram, cache: HashMap::new() };
    let mut best: Vec<Vec<State>> = vec![Vec::new(); raw.len() + 1];
    best[0].push(State {
        score: 0.0,
        path: Vec::new(),
    });
    for j in 1..=raw.len() {
        let mut cands: Vec<State> = Vec::new();
        // 隔音符不占音节:让上一位置的状态直接越过它,词与词才能在 `'` 两侧相接。
        if raw.as_bytes()[j - 1] == b'\'' {
            cands.extend(best[j - 1].iter().cloned());
        }
        for node in &by_end[j] {
            for st in &best[node.start] {
                let prev = st.path.last().map_or("", |p| p.0.as_str());
                let mut path = st.path.clone();
                path.push((node.word.clone(), node.spans.clone()));
                cands.push(State {
                    score: st.score + node.score + gram.bonus(prev, &node.word),
                    path,
                });
            }
        }
        cands.sort_by(|a, b| b.score.total_cmp(&a.score));
        cands.truncate(BEAM);
        best[j] = cands;
    }

    let mut cands: Vec<Cand> = Vec::new();
    for st in &best[raw.len()] {
        let text: String = st.path.iter().map(|p| p.0.as_str()).collect();
        if text.is_empty() {
            continue;
        }
        let spans: Vec<(usize, usize)> = st.path.iter().flat_map(|p| p.1.iter().copied()).collect();
        cands.push(Cand {
            text,
            consume: spans.len(),
            score: st.score,
            full: true,
            words: st.path.len(),
            spans,
        });
    }
    // 单词候选:只取从输入开头起的词(选中后剩余输入继续组合)。
    for node in by_end.iter().flatten() {
        if node.start != 0 {
            continue;
        }
        cands.push(Cand {
            text: node.word.clone(),
            consume: node.spans.len(),
            score: node.score,
            full: false,
            words: 1,
            spans: node.spans.clone(),
        });
    }
    cands.sort_by(|a, b| {
        b.full
            .cmp(&a.full)
            .then(b.score.total_cmp(&a.score))
            .then(a.consume.cmp(&b.consume))
            .then(a.text.cmp(&b.text))
    });
    let mut seen: HashSet<(String, usize)> = HashSet::new();
    cands.retain(|c| seen.insert((c.text.clone(), c.consume)));
    cands.truncate(MAX_CANDS);
    let display = cands
        .first()
        .map(|c| c.text.clone())
        .unwrap_or_else(|| raw.to_string());
    Built { cands, display }
}
