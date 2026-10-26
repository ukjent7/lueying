//! 候选生成:原始输入上的音节图 + 词图 Viterbi 解码。
//!
//! 词图按**原始字节位置**建:同一串输入的所有切分天然并存,不再枚举切分路径,
//! 也不会因为路径数上限漏掉正确切分。整句分 = Σ 词节点分 + Σ 搭配分。
//!
//! 词节点分把词频归一成对数概率(`ln(freq/total)`),再减每词固定罚:拆成更多
//! 高频碎片的路径要额外付出每词一份的归一化代价。搭配分来自 rime `.gram`
//! 模型(可缺省),按 librime-octagram 的搭配/未搭配/句末罚参与整句排序。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use lueying_dict::gram::{CONTEXT_CHARS, Gram};
use lueying_dict::syllables::{self, Fuzzy};
use lueying_dict::{Dictionary, signature};

use crate::segment::{self, Syll};
use crate::typo;
use crate::user::UserDict;

/// 展示给上层的候选。
#[derive(Clone, Debug)]
pub struct Candidate {
    pub text: String,
    /// 候选文本的切词结果;单个前缀词就是它本身。
    pub words: Vec<String>,
    /// 吃掉的音节数。
    pub consume: usize,
    pub score: f64,
    /// 各音节的原始字节区间(回删与逐段上屏用)。
    pub spans: Vec<(usize, usize)>,
    /// 覆盖整个组合输入,而不是前缀词或联想补全。
    pub full: bool,
    /// 逐音节精确命中(含简拼),未经模糊扩写;用户词不算。
    pub trusted: bool,
    /// 两个及以上精确词典词拼成的整句。没有单词条频次,保护时另设门槛。
    pub sentence: bool,
    /// 模糊改动处数;0 表示逐音节精确命中。
    pub edits: usize,
    /// 词典词频(用户独有词为 0)。精确首位保护只锁有 corpus 依据的常用词。
    pub freq: u64,
    /// 同一按键下用户选过该词的次数。
    pub user_freq: u32,
    /// 命中的模糊组位掩码(0 表示精确)。
    pub groups: u8,
    /// 模糊候选的注音提示(如 `shi`);精确候选为空。
    pub hint: Option<String>,
    /// 纠正候选的学习键(原串与纠正串不同,不能从 `spans` 反推)。
    pub learn_key: Option<String>,
    /// 快捷输入(日期/时间/数字金额):不写词库,也不参与词级转移。
    pub quick: bool,
    /// 用户在同键下跳过过该候选:只小幅降权,精确首位保护也会让位。
    pub rejected: bool,
}

/// 一次构建的产物。
pub struct Built {
    pub cands: Vec<Candidate>,
    /// 内联显示文本:首选候选文本,无候选时原文。
    pub display: String,
    /// 能接长,且不是「全拼音节后面又接简拼」的伪前缀。
    ///
    /// 签名只取首字母:`nihap` 会撞上「你害怕」的 `nhp`,但 `ha` 后面已经
    /// 跟了 `p`,不可能再补成 `hai`。末单元是全拼时还能继续打长该音节,
    /// 全简拼时也能继续补简拼,这两种才算真的还在输入。
    pub safe_extend: bool,
    /// 当前输入是某个词典键的逐字前缀(末音节可以没打完)。
    pub typing_prefix: bool,
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
/// 2.2 在项目、外部手写、白霜脚本和中立难例上同时不退步。
const WORD_PENALTY: f64 = 2.2;
/// 整句 Viterbi 每档保留几条路径。3 条时模糊扩写会把正确切分挤出束,
/// 与 librime/清风同为 7 条,长句仍只保留常数级状态。
const BEAM: usize = 7;
/// 建图访问预算:简拼混合输入的组合可能很多,超出后停止扩词。
const NODE_BUDGET: usize = 4096;
/// 每处模糊改动扣的对数概率,等同词频减半;与青简、清风及 librime/libime 同轴。
const FUZZY_PENALTY: f64 = std::f64::consts::LN_2;
/// 精确首位保护的最低词频。只要词库给出非零频次,就优先保住用户实际打出的
/// 全拼整词;零频长尾仍按分数竞争,避免把无意义词条顶到首位。
const EXACT_PROTECT_MIN_FREQ: u64 = 1;
/// 整句精确保护允许的落后幅度(对数概率)。整句没有词典频次背书,只有分数
/// 与首位接近时才说明两种读法都成立;差距过大留给模糊读法。
const EXACT_SENTENCE_MARGIN: f64 = 3.0;
/// 签名翻转变体上限;超过后只保留原键与单点翻转,保证至少有一条召回。
const MAX_SIG_VARIANTS: usize = 32;
/// 全拼模糊组合的键数上限;按改动处数截断,原键与单点模糊优先保留。
const MAX_FUZZY_KEYS: usize = 64;

#[derive(Clone)]
struct Hit {
    word: String,
    /// 构造时算好(排序/回溯反复取分,不能每次都算 ln)。
    score: f64,
    /// 逐音节精确命中(含简拼),未经模糊扩写。
    trusted: bool,
    /// 模糊改动处数。
    edits: usize,
    /// 词典词频;用户独有词为 0。
    freq: u64,
    /// 同一按键下用户选过该词的次数。
    user_freq: u32,
    /// 命中的模糊组位掩码。
    groups: u8,
    /// 模糊候选的注音提示。
    hint: Option<String>,
}

impl Hit {
    /// 词典词条。白霜有大量权重 0 的长尾词,按频次 1 计,天然沉底。
    #[allow(clippy::too_many_arguments)]
    fn dict(
        word: String,
        freq: u64,
        user_freq: u32,
        total: f64,
        edits: usize,
        trusted: bool,
        groups: u8,
        hint: Option<String>,
    ) -> Self {
        let mut hit = Self::raw(word, freq.max(1) as f64, user_freq, total, edits);
        hit.trusted = trusted;
        hit.freq = freq;
        hit.groups = groups;
        hit.hint = hint;
        hit
    }

    /// 纯用户词(词典没有):以百万级词频为基准,学习过的词迅速领先。
    fn user_only(word: String, user_freq: u32, total: f64, edits: usize, groups: u8) -> Self {
        let mut hit = Self::raw(word, 1_000_000.0, user_freq, total, edits);
        hit.groups = groups;
        hit
    }

    fn raw(word: String, base: f64, user_freq: u32, total: f64, edits: usize) -> Self {
        let chars = word.chars().count().max(1) as f64;
        Self {
            score: (base / total).ln()
                + CHAR_BONUS * chars
                + USER_W * (1.0 + f64::from(user_freq)).ln()
                - WORD_PENALTY
                - FUZZY_PENALTY * edits as f64,
            word,
            trusted: false,
            edits,
            freq: 0,
            user_freq,
            groups: 0,
            hint: None,
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

/// 候选吃掉的那些原始输入片段拼成用户词典键(`ni'hao`)。
pub(crate) fn spans_key(raw: &str, spans: &[(usize, usize)]) -> String {
    spans
        .iter()
        .map(|&(start, end)| syllables::canonical(&raw[start..end]))
        .collect::<Vec<_>>()
        .join("'")
}

/// 一个音节的模糊展开:变体键、改动处数、命中组位。
type Expansions = Rc<Vec<(String, usize, u8)>>;

/// 用户键里的简拼单元:单字母,或 zh/ch/sh。
fn is_abbrev_key(e: &str) -> bool {
    e.chars().count() == 1 || matches!(e, "zh" | "ch" | "sh")
}

/// 词条键是否逐音节匹配输入;命中时返回模糊改动处数。
///
/// 简拼按首字母前缀匹配,n/l、f/h、r/l 等改首字母的模糊组也在这里放宽;
/// 全拼按等价音节匹配,处数由声母、韵母各算一处。`entry_may_abbrev` 用于
/// 用户键:学习时可能用简拼、查询时用全拼,两边都要按同一音节识别。
fn match_edits(
    entry_key: &str,
    sylls: &[Syll],
    entry_may_abbrev: bool,
    fuzzy: &Fuzzy,
    exp_cache: &mut HashMap<String, Expansions>,
) -> Option<(usize, u8)> {
    let mut edits = 0;
    let mut groups = 0;
    let mut parts = entry_key.split('\'');
    for t in sylls {
        let e = parts.next()?;
        // 学习时用简拼、现在用全拼(或反过来)是同一个词,不该丢掉提频。
        // 简拼只保留音节首字母,首字母相同就视为本音节,模糊等价才计一处改动。
        let typed_first = t.text.chars().next()?;
        let found_first = e.chars().next()?;
        if t.abbrev {
            if e.starts_with(t.text.as_str())
                || (entry_may_abbrev && is_abbrev_key(e) && typed_first == found_first)
            {
                continue;
            }
            if fuzzy.is_off() {
                return None;
            }
            let group = syllables::initial_group(typed_first, found_first, fuzzy)?;
            if group == 0 {
                return None;
            }
            edits += 1;
            groups |= group;
            continue;
        }
        if e == t.text {
            continue;
        }
        if entry_may_abbrev && is_abbrev_key(e) {
            // `z` 之于 `zhi`、`zh` 之于 `zhi` 都是同一音节的简拼;`zh` 之于
            // `zi` 也是简拼没写全,不能按模糊改动罚分。词典键必须按完整音节
            // 比较,否则单韵母词(啊/饿/哦)会错误命中 `ai/ei/ou`。
            if t.text.starts_with(e) || typed_first == found_first {
                continue;
            }
            if fuzzy.is_off() {
                return None;
            }
            let group = syllables::initial_group(typed_first, found_first, fuzzy)?;
            if group == 0 {
                return None;
            }
            edits += 1;
            groups |= group;
            continue;
        }
        if fuzzy.is_off() {
            return None;
        }
        let exps = exp_cache
            .entry(t.text.clone())
            .or_insert_with(|| Rc::new(syllables::expansions_scored(&t.text, fuzzy)));
        let (cost, group) = exps
            .iter()
            .find(|item| item.0.as_str() == e)
            .map(|item| (item.1, item.2))?;
        edits += cost;
        groups |= group;
    }
    parts.next().is_none().then_some((edits, groups))
}

/// 同一个用户词可能以全拼、简拼或模糊键各学过一次;查询按当前输入取最有力的一条。
struct UserMatch {
    freq: u32,
    edits: usize,
    groups: u8,
    key: String,
}

fn user_freq(users: &HashMap<String, UserMatch>, word: &str) -> u32 {
    users.get(word).map_or(0, |m| m.freq)
}

struct Querier<'a> {
    dict: &'a Dictionary,
    user: &'a UserDict,
    fuzzy: Fuzzy,
    total: f64,
    cache: HashMap<String, Rc<Vec<Hit>>>,
    /// 与 `cache` 同键:当前输入能匹配上的用户词及最有力的一条记录。
    user_cache: HashMap<String, Rc<HashMap<String, UserMatch>>>,
    exp_cache: HashMap<String, Expansions>,
}

impl Querier<'_> {
    fn lookup(&mut self, sylls: &[Syll]) -> Rc<Vec<Hit>> {
        let key = typed_key(sylls);
        if let Some(hits) = self.cache.get(&key) {
            return Rc::clone(hits);
        }
        let (hits, users) = self.compute(sylls, &key);
        self.user_cache.insert(key.clone(), Rc::new(users));
        let hits = Rc::new(hits);
        self.cache.insert(key, Rc::clone(&hits));
        hits
    }

    fn compute(
        &mut self,
        sylls: &[Syll],
        key: &str,
    ) -> (Vec<Hit>, HashMap<String, UserMatch>) {
        let any_abbrev = sylls.iter().any(|s| s.abbrev);
        let fuzzy_on = !self.fuzzy.is_off();
        // 用户词先按音节数与首字母收窄,再做逐音节匹配。词典命中也要用这份
        // 结果提频:同一个词用简拼学过,换成全拼输入时仍然应该靠前。
        let mut users: HashMap<String, UserMatch> = HashMap::new();
        if !self.user.is_empty() {
            let first = sylls
                .first()
                .and_then(|s| s.text.chars().next())
                .unwrap_or('\0');
            let shapes: Vec<(usize, char)> = syllables::initial_alternatives(first, &self.fuzzy)
                .into_iter()
                .map(|c| (sylls.len(), c))
                .collect();
            let user_dict = self.user;
            let fuzzy = self.fuzzy;
            let exp_cache = &mut self.exp_cache;
            user_dict.for_each_matching(&shapes, |ukey, uword, ufreq| {
                let Some((edits, groups)) = match_edits(ukey, sylls, true, &fuzzy, exp_cache)
                else {
                    return;
                };
                if let Some(current) = users.get_mut(uword) {
                    if ufreq > current.freq || (ufreq == current.freq && edits < current.edits) {
                        current.freq = ufreq;
                        current.edits = edits;
                        current.groups = groups;
                        current.key.clear();
                        current.key.push_str(ukey);
                    }
                } else {
                    users.insert(
                        uword.to_string(),
                        UserMatch {
                            freq: ufreq,
                            edits,
                            groups,
                            key: ukey.to_string(),
                        },
                    );
                }
            });
        }
        let mut hits: Vec<Hit> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        // 精确整键走专用索引:最快,也是 `trusted` 的证据来源。
        if !any_abbrev {
            for e in self.dict.lookup_exact(key) {
                let word = e.word.to_string();
                if seen.insert((word.clone(), e.key.to_string())) {
                    hits.push(Hit::dict(
                        word,
                        e.freq,
                        user_freq(&users, e.word),
                        self.total,
                        0,
                        true,
                        0,
                        None,
                    ));
                }
            }
        }
        // 简拼走签名区间;全拼模糊按等价音节枚举精确键。后者只做几次二分,
        // 不用为每个首字母翻转扫整段签名,长句输入也保持首键延迟。
        if any_abbrev {
            let base = signature(key);
            for sig in self.sig_variants(&base, sylls) {
                for e in self.dict.lookup_sig(&sig, SCAN_VISIT_CAP) {
                    if !seen.insert((e.word.to_string(), e.key.to_string())) {
                        continue;
                    }
                    let Some((edits, groups)) =
                        match_edits(e.key, sylls, false, &self.fuzzy, &mut self.exp_cache)
                    else {
                        continue;
                    };
                    hits.push(Hit::dict(
                        e.word.to_string(),
                        e.freq,
                        user_freq(&users, e.word),
                        self.total,
                        edits,
                        edits == 0,
                        groups,
                        (edits > 0).then(|| e.key.replace('\'', " ")),
                    ));
                }
            }
        } else if fuzzy_on {
            for (variant, edits, groups) in self.expanded_keys(sylls) {
                if variant == key {
                    continue;
                }
                for e in self.dict.lookup_exact(&variant) {
                    if !seen.insert((e.word.to_string(), e.key.to_string())) {
                        continue;
                    }
                    hits.push(Hit::dict(
                        e.word.to_string(),
                        e.freq,
                        user_freq(&users, e.word),
                        self.total,
                        edits,
                        false,
                        groups,
                        Some(e.key.replace('\'', " ")),
                    ));
                }
            }
        }
        // 用户词并入:词典已有的词已带上提频,只补词典没有的。
        let total = self.total;
        let mut hit_words: HashSet<String> = hits.iter().map(|h| h.word.clone()).collect();
        for (uword, m) in &users {
            if hit_words.insert(uword.clone()) {
                let mut hit = Hit::user_only(uword.clone(), m.freq, total, m.edits, m.groups);
                if m.edits > 0 {
                    hit.hint = Some(m.key.replace('\'', " "));
                }
                hits.push(hit);
            }
        }
        // 组级学习只降低该组的模糊罚分,不改精确命中的 `trusted` 保护。
        for hit in &mut hits {
            if hit.groups != 0 {
                hit.score += self.user.group_bonus(hit.groups);
            }
        }
        (hits, users)
    }

    /// 当前输入是否是某个词典键的逐字前缀。末音节没打完时键里没有结尾
    /// 隔音符,`has_key_prefix` 仍能命中,用来避免把半截输入当成手误。
    fn is_typing_prefix(&mut self, sylls: &[Syll]) -> bool {
        let key = typed_key(sylls);
        if self.dict.has_key_prefix(&key) {
            return true;
        }
        if self.fuzzy.is_off() {
            return false;
        }
        let keys = self.expanded_keys(sylls);
        keys.iter().any(|(key, _, _)| self.dict.has_key_prefix(key))
    }

    /// 还能不能接更长的词:用键/签名前缀判断,避免无意义的组合爆炸。
    /// 模糊等价音会改变键面,所以逐一检查等价键前缀。
    fn may_extend(&mut self, sylls: &[Syll]) -> bool {
        if sylls.iter().any(|s| s.abbrev) {
            let key = typed_key(sylls);
            return self
                .sig_variants(&signature(&key), sylls)
                .iter()
                .any(|sig| self.dict.has_sig_prefix(sig));
        }
        let mut prefix = typed_key(sylls);
        prefix.push('\'');
        if self.dict.has_key_prefix(&prefix) {
            return true;
        }
        if self.fuzzy.is_off() {
            return false;
        }
        let keys = self.expanded_keys(sylls);
        keys.iter().any(|(key, _, _)| {
            let mut prefix = key.clone();
            prefix.push('\'');
            self.dict.has_key_prefix(&prefix)
        })
    }

    /// 全拼模糊召回:逐音节展开等价音节,组合成精确键再查主索引。
    ///
    /// 只做字符串组合与二分,不像签名扫描那样把同首字母的整段词条都读一遍。
    /// 组合数超过 [`MAX_FUZZY_KEYS`] 时按改动处数从少到多截断,保证原键与
    /// 单点模糊仍在。
    fn expanded_keys(&mut self, sylls: &[Syll]) -> Vec<(String, usize, u8)> {
        if sylls.iter().any(|s| s.abbrev) {
            return vec![(typed_key(sylls), 0, 0)];
        }
        let fuzzy = self.fuzzy;
        let mut out = vec![(String::new(), 0usize, 0u8)];
        for t in sylls {
            let exps = self
                .exp_cache
                .entry(t.text.clone())
                .or_insert_with(|| Rc::new(syllables::expansions_scored(&t.text, &fuzzy)));
            let mut next = Vec::with_capacity(out.len() * exps.len());
            for (prefix, cost, groups) in &out {
                for (text, extra, group) in exps.iter() {
                    let mut key = String::with_capacity(prefix.len() + text.len() + 1);
                    key.push_str(prefix);
                    if !prefix.is_empty() {
                        key.push('\'');
                    }
                    key.push_str(text);
                    next.push((key, cost + extra, *groups | *group));
                }
            }
            out = next;
            if out.len() > MAX_FUZZY_KEYS {
                out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
                out.truncate(MAX_FUZZY_KEYS);
            }
        }
        out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        out.dedup_by(|a, b| {
            if a.0 == b.0 {
                b.2 |= a.2;
                true
            } else {
                false
            }
        });
        out
    }

    /// 改首字母的模糊组会改变签名:枚举各位置的首字母等价集。
    ///
    /// 单点翻转全部保留;多点组合最多 [`MAX_SIG_VARIANTS`] 个,超出后
    /// 只保留原键与单点翻转,保证四个以上音节的输入仍能召回一次模糊。
    fn sig_variants(&self, base: &str, sylls: &[Syll]) -> Vec<String> {
        let base_chars: Vec<char> = base.chars().collect();
        if self.fuzzy.is_off() || base_chars.len() != sylls.len() {
            return vec![base.to_string()];
        }
        let mut positions: Vec<(usize, Vec<char>)> = Vec::new();
        for (i, s) in sylls.iter().enumerate() {
            let Some(c) = s.text.chars().next() else {
                continue;
            };
            let alternatives = syllables::initial_alternatives(c, &self.fuzzy);
            if alternatives.len() > 1 {
                positions.push((i, alternatives));
            }
        }
        if positions.is_empty() {
            return vec![base.to_string()];
        }
        let mut out = vec![base.to_string()];
        for (i, alternatives) in &positions {
            let mut next = Vec::new();
            for sig in &out {
                for &c in alternatives {
                    if sig.chars().nth(*i) == Some(c) {
                        continue;
                    }
                    let mut chars: Vec<char> = sig.chars().collect();
                    chars[*i] = c;
                    next.push(chars.into_iter().collect::<String>());
                }
            }
            out.extend(next);
            out.sort();
            out.dedup();
            if out.len() > MAX_SIG_VARIANTS {
                // 预算用尽:退回原键 + 每个位置的单点翻转。
                let mut capped = vec![base.to_string()];
                for (pos, alternatives) in &positions {
                    for &c in alternatives {
                        if c == base_chars[*pos] {
                            continue;
                        }
                        let mut chars = base_chars.clone();
                        chars[*pos] = c;
                        capped.push(chars.into_iter().collect());
                    }
                }
                capped.sort();
                capped.dedup();
                return capped;
            }
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
    trusted: bool,
    edits: usize,
    freq: u64,
    user_freq: u32,
    groups: u8,
    hint: Option<String>,
}

/// 解码路径的一步:词文本 + 各音节原始字节区间。
type Step = (String, Vec<(usize, usize)>);
type Path = Vec<Step>;

/// 搭配分:前文尾 -> 当前词的 `.gram` 打分。查询结果按 (前文,词,句末) 缓存。
struct Grammar<'a> {
    gram: Option<&'a Gram>,
    cache: &'a mut HashMap<(String, String, bool), f64>,
}

impl Grammar<'_> {
    fn bonus(&mut self, context: &str, word: &str, is_rear: bool) -> f64 {
        let Some(gram) = self.gram else {
            return 0.0;
        };
        if context.is_empty() {
            return 0.0;
        }
        let key = (context.to_string(), word.to_string(), is_rear);
        if let Some(v) = self.cache.get(&key) {
            return *v;
        }
        let v = gram.score(context, word, is_rear);
        self.cache.insert(key, v);
        v
    }
}

/// 前文尾部最多 `CONTEXT_CHARS` 个字,供语法模型查搭配。
fn context_tail(path: &[Step]) -> String {
    let mut chars: Vec<char> = Vec::with_capacity(CONTEXT_CHARS);
    for (word, _) in path.iter().rev() {
        for ch in word.chars().rev() {
            chars.push(ch);
            if chars.len() == CONTEXT_CHARS {
                chars.reverse();
                return chars.into_iter().collect();
            }
        }
    }
    chars.reverse();
    chars.into_iter().collect()
}

/// 词枚举的游标状态。`finish[p]` 表示 `raw[p..]` 能否被音节图完整切分:
/// 词尾落在切不动的位置(如 `si` 里只认 `s` 的「是」)不产生候选,
/// 残留的字母永远无法上屏。
struct Builder<'a, 'd> {
    q: &'a mut Querier<'d>,
    edges: &'a [Vec<Syll>],
    raw: &'a str,
    finish: &'a [bool],
    by_end: &'a mut [Vec<Node>],
    units: Vec<Syll>,
    spans: Vec<(usize, usize)>,
    visits: usize,
    safe_extend: bool,
    typing: bool,
}

impl Builder<'_, '_> {
    /// 从 `pos` 起深度优先枚举词,写到 `by_end[词尾]`。
    fn run(&mut self, root: usize, pos: usize) {
        if !self.units.is_empty() {
            if !self.finish[pos] {
                return;
            }
            let hits = self.q.lookup(&self.units);
            for h in hits.iter() {
                self.by_end[pos].push(Node {
                    start: root,
                    word: h.word.clone(),
                    score: h.score(),
                    spans: self.spans.clone(),
                    trusted: h.trusted,
                    edits: h.edits,
                    freq: h.freq,
                    user_freq: h.user_freq,
                    groups: h.groups,
                    hint: h.hint.clone(),
                });
            }
            self.visits += 1;
            if pos == self.raw.len() && self.q.is_typing_prefix(&self.units) {
                self.typing = true;
            }
            if self.units.len() >= MAX_WORD_SYL || self.visits > NODE_BUDGET {
                return;
            }
            if self.q.may_extend(&self.units) {
                if pos == self.raw.len()
                    && (self.units.last().is_some_and(|s| !s.abbrev)
                        || self.units.iter().all(|s| s.abbrev))
                {
                    self.safe_extend = true;
                }
            } else {
                return;
            }
        }
        if pos >= self.raw.len() {
            return;
        }
        if self.raw.as_bytes()[pos] == b'\'' {
            // 隔音符是硬边界:词可以跨过它,但不产生音节。
            self.run(root, pos + 1);
            return;
        }
        for i in 0..self.edges[pos].len() {
            let e = self.edges[pos][i].clone();
            self.units.push(e.clone());
            self.spans.push(e.span);
            self.run(root, e.span.1);
            self.spans.pop();
            self.units.pop();
        }
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
    path: Path,
    /// 整条路径由单个精确词典词构成时,才允许让模型让位。
    trusted: bool,
    /// 路径由两个及以上精确词拼成。
    sentence: bool,
    /// 路径累计的模糊改动处数。
    edits: usize,
    /// 单节点路径的词典词频;多词拼句为 0。
    freq: u64,
    /// 路径累计的模糊组。
    groups: u8,
    /// 单节点路径的注音提示;多词拼句为空。
    hint: Option<String>,
}

fn build_exact(
    dict: &Dictionary,
    user: &UserDict,
    fuzzy: &Fuzzy,
    gram: Option<&Gram>,
    cache: &mut HashMap<(String, String, bool), f64>,
    context: &str,
    previous2: Option<&str>,
    previous: Option<&str>,
    raw: &str,
) -> Built {
    let total = dict.total_weight() as f64;
    let mut q = Querier {
        dict,
        user,
        fuzzy: *fuzzy,
        total,
        cache: HashMap::new(),
        user_cache: HashMap::new(),
        exp_cache: HashMap::new(),
    };
    let edges = segment::edges(raw);
    // finish[p]:raw[p..] 能否被音节图切完。词尾落在切不动的位置时,剩下
    // 的字母永远上不了屏,这类词节点直接不要。
    let mut finish = vec![false; raw.len() + 1];
    finish[raw.len()] = true;
    for i in (0..raw.len()).rev() {
        let ok = if raw.as_bytes()[i] == b'\'' {
            finish[i + 1]
        } else {
            edges[i].iter().any(|e| finish[e.span.1])
        };
        finish[i] = ok;
    }
    let mut by_end: Vec<Vec<Node>> = vec![Vec::new(); raw.len() + 1];
    let safe_extend;
    let typing;
    {
        let mut b = Builder {
            q: &mut q,
            edges: &edges,
            raw,
            finish: &finish,
            by_end: &mut by_end,
            units: Vec::new(),
            spans: Vec::new(),
            visits: 0,
            safe_extend: false,
            typing: false,
        };
        for (i, &ok) in finish.iter().take(raw.len()).enumerate() {
            if raw.as_bytes()[i] == b'\'' || !ok {
                continue;
            }
            b.units.clear();
            b.spans.clear();
            b.run(i, i);
        }
        safe_extend = b.safe_extend;
        typing = b.typing;
    }
    for list in &mut by_end {
        cap_nodes(list);
    }

    let mut gram = Grammar { gram, cache };
    let mut best: Vec<Vec<State>> = vec![Vec::new(); raw.len() + 1];
    best[0].push(State {
        score: 0.0,
        path: Vec::new(),
        trusted: true,
        sentence: false,
        edits: 0,
        freq: 0,
        groups: 0,
        hint: None,
    });
    for j in 1..=raw.len() {
        let mut cands: Vec<State> = Vec::new();
        // 隔音符不占音节:让上一位置的状态直接越过它,词与词才能在 `'` 两侧相接。
        if raw.as_bytes()[j - 1] == b'\'' {
            cands.extend(best[j - 1].iter().cloned());
        }
        for node in &by_end[j] {
            for st in &best[node.start] {
                let tail = context_tail(&st.path);
                let mut path = st.path.clone();
                path.push((node.word.clone(), node.spans.clone()));
                cands.push(State {
                    score: st.score + node.score + gram.bonus(&tail, &node.word, j == raw.len()),
                    path,
                    // 只有单节点整句算词典精确命中;多词拼出的句子没有这份依据。
                    trusted: st.path.is_empty() && node.trusted,
                    sentence: !st.path.is_empty() && st.edits + node.edits == 0,
                    edits: st.edits + node.edits,
                    freq: if st.path.is_empty() { node.freq } else { 0 },
                    groups: st.groups | node.groups,
                    hint: if st.path.is_empty() {
                        node.hint.clone()
                    } else {
                        None
                    },
                });
            }
        }
        cands.sort_by(|a, b| b.score.total_cmp(&a.score));
        cands.truncate(BEAM);
        best[j] = cands;
    }

    let mut full: Vec<Candidate> = Vec::new();
    for st in &best[raw.len()] {
        let text: String = st.path.iter().map(|p| p.0.as_str()).collect();
        if text.is_empty() {
            continue;
        }
        let spans: Vec<(usize, usize)> = st.path.iter().flat_map(|p| p.1.iter().copied()).collect();
        let key = spans_key(raw, &spans);
        let reject = user.reject_penalty(&key, &text);
        full.push(Candidate {
            text: text.clone(),
            words: st.path.iter().map(|p| p.0.clone()).collect(),
            consume: spans.len(),
            score: st.score - reject,
            spans,
            full: true,
            trusted: st.trusted,
            sentence: st.sentence,
            edits: st.edits,
            freq: st.freq,
            user_freq: q
                .user_cache
                .get(&key)
                .and_then(|users| users.get(&text))
                .map_or(0, |m| m.freq),
            groups: st.groups,
            hint: st.hint.clone(),
            learn_key: None,
            quick: false,
            rejected: reject > 0.0,
        });
    }
    // 个人上下文只调整整句候选的顺序:词频、模糊罚分和语法模型已经决定了
    // 进入 beam 的路径,这里再让用户常打的前后搭配与词序在同一批路径里靠前。
    for cand in &mut full {
        cand.score += user.context_bonus(context, &cand.text);
        cand.score += user.transition_bonus(previous, &cand.words);
        cand.score += user.trigram_bonus(previous2, previous, &cand.words);
    }
    full.sort_by(|a, b| b.score.total_cmp(&a.score));
    // 单词候选:只取从输入开头起的词(选中后剩余输入继续组合)。
    let mut words: Vec<Candidate> = Vec::new();
    for (end, nodes) in by_end.iter().enumerate() {
        for node in nodes {
            if node.start != 0 {
                continue;
            }
            let reject = user.reject_penalty(&spans_key(raw, &node.spans), &node.word);
            words.push(Candidate {
                text: node.word.clone(),
                words: vec![node.word.clone()],
                consume: node.spans.len(),
                score: node.score - reject,
                spans: node.spans.clone(),
                full: end == raw.len(),
                trusted: node.trusted,
                sentence: false,
                edits: node.edits,
                freq: node.freq,
                user_freq: node.user_freq,
                groups: node.groups,
                hint: node.hint.clone(),
                learn_key: None,
                quick: false,
                rejected: reject > 0.0,
            });
        }
    }
    // 前缀词也吃同一份个人上下文,逐段上屏时第一段就能按习惯排序。
    for cand in &mut words {
        cand.score += user.context_bonus(context, &cand.text);
        cand.score += user.transition_bonus(previous, &cand.words);
        cand.score += user.trigram_bonus(previous2, previous, &cand.words);
    }
    words.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.consume.cmp(&b.consume))
            .then(a.text.cmp(&b.text))
    });
    // 首页优先放满整句备选:长句的正确读法常排在第 6-8 条,第二位的分段
    // 前缀会把它挤出页外。五条名额先给整句,最高分前缀词顺延到第 6 位;
    // 需要逐段上屏时下移一格即可,整句不足五条时前缀词自动补位。
    let mut cands: Vec<Candidate> = Vec::new();
    let mut rest = full.into_iter();
    cands.extend(rest.by_ref().take(5));
    let taken: HashSet<String> = cands.iter().map(|c| c.text.clone()).collect();
    let mut words = words.into_iter();
    // 第 6 位的分段入口不能与已收录的整句重复;重复项留着也会被去重掉。
    if let Some(prefix) = words.find(|w| !taken.contains(&w.text)) {
        cands.push(prefix);
    }
    cands.extend(rest);
    cands.extend(words);
    let mut seen: HashSet<(String, usize)> = HashSet::new();
    cands.retain(|c| seen.insert((c.text.clone(), c.consume)));
    protect_exact(&mut cands);
    cands.truncate(MAX_CANDS);
    let display = cands
        .first()
        .map(|c| c.text.clone())
        .unwrap_or_else(|| raw.to_string());
    Built {
        cands,
        display,
        safe_extend,
        typing_prefix: typing,
    }
}

/// 错音纠正的总预算。最长 24 键最多 400 条纠正,每条都要整图解码;
/// 慢机器上全部跑完会把首键拖到几百毫秒。按时间截断,常见手误在前几条
/// 就能命中,正常输入完全不受影响。
const TYPO_BUDGET: Duration = Duration::from_millis(6);

/// 候选生成入口:正常解码没有整句候选时,尝试一次相邻键/漏键纠正。
///
/// 纠正只在精确与模糊都出不了整句时发生,避免影响正常输入。同一手误可能对应
/// 多条可切分的纠正串(`hoa` 既能补成 `oha` 也能调换成 `hao`),这里取候选
/// 打分最高的一条,不先到先得。纠正串可能与原串长度不同,候选按整串上屏,
/// 学习键单独记录纠正后的音节串。
pub fn build(
    dict: &Dictionary,
    user: &UserDict,
    fuzzy: &Fuzzy,
    gram: Option<&Gram>,
    cache: &mut HashMap<(String, String, bool), f64>,
    context: &str,
    previous2: Option<&str>,
    previous: Option<&str>,
    raw: &str,
) -> Built {
    let built = build_exact(
        dict,
        user,
        fuzzy,
        gram,
        cache,
        context,
        previous2,
        previous,
        raw,
    );
    if built.cands.iter().any(|c| c.full)
        || built.safe_extend
        || built.typing_prefix
        || crate::quick::is_trigger(raw)
        || !typo::eligible(raw)
    {
        return built;
    }
    let deadline = Instant::now() + TYPO_BUDGET;
    // 同一手误可能对应多条可切分纠正串;保留打分最高的一条,先到先得会把
    // `hoa` 补成「哦哈」而不是「好」。分数已含词频、每词罚分与搭配分,
    // 不同纠正串之间可以直接比较。
    let mut best: Option<Built> = None;
    let mut best_score = f64::NEG_INFINITY;
    for correction in typo::variants(raw) {
        if Instant::now() >= deadline {
            break;
        }
        if !segment::complete(&correction) {
            continue;
        }
        let mut fixed = build_exact(
            dict,
            user,
            fuzzy,
            gram,
            cache,
            context,
            previous2,
            previous,
            &correction,
        );
        let mut kept = Vec::new();
        for mut cand in fixed.cands.drain(..) {
            if !cand.full {
                continue;
            }
            let key = spans_key(&correction, &cand.spans);
            // 纠正串与原串长度可能不同,音节区间无法可靠映射;整串上屏,
            // 学习键单独保存纠正后的音节串。
            cand.spans = vec![(0, raw.len())];
            cand.consume = 1;
            cand.trusted = false;
            cand.sentence = false;
            cand.edits = 0;
            cand.groups = 0;
            cand.hint = Some(format!("纠正 {}", key.replace('\'', " ")));
            cand.learn_key = Some(key);
            kept.push(cand);
        }
        if kept.is_empty() {
            continue;
        }
        kept.sort_by(|a, b| b.score.total_cmp(&a.score));
        let score = kept[0].score;
        let mut seen = HashSet::new();
        kept.retain(|c| seen.insert((c.text.clone(), c.consume)));
        kept.truncate(MAX_CANDS);
        fixed.cands = kept;
        fixed.display = fixed
            .cands
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default();
        if score > best_score {
            best_score = score;
            best = Some(fixed);
        }
    }
    best.unwrap_or(built)
}

/// 排序后的最后一道闸:没打错音时,常用精确整词固定首位。
///
/// 只锁首位,不锁整页。青简把全部精确候选压在模糊前,模糊可能永远翻不到;
/// 完全按分数排,80 倍词频差又会让高频模糊词压过精确词。这里取中间:
/// 精确常用词占第 1 位,模糊候选从第 2 位开始按分数竞争。
///
/// 学习覆盖优先于精确保护:同一按键下选过一次的词直接回到首位,与微软
/// 拼音、微信一致;选错了可以 Shift+Delete 忘掉。模糊词同样适用,用户
/// 既然选了它,就说明这个音的分歧对他没有意义。
///
/// 被跳过过的候选不享受这两层保护:负分已经说明它不符合当前意图,
/// 再按词频或精确命中把它顶回首位会让跳过学习失去作用。
///
/// 多词整句也保护,但只在分数与首位接近时生效:整句没有单词条频次,
/// 勉强成立的精确切分(如 `zongguo` 拼出「总过」)不该顶掉真正的模糊词。
fn protect_exact(cands: &mut [Candidate]) {
    if cands.is_empty() {
        return;
    }
    // 学过的前缀词不能顶掉整句:回删分段上屏很常见,整句首选仍应由整句候选竞争。
    let has_full = cands.iter().any(|c| c.full);
    if let Some(index) = cands
        .iter()
        .enumerate()
        .filter(|(_, c)| c.user_freq > 0 && !c.rejected && (c.full || !has_full))
        .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
        .map(|(i, _)| i)
    {
        cands[..=index].rotate_right(1);
        return;
    }
    let leader = cands[0].score;
    if let Some(index) = cands
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            !c.rejected
                && c.full
                && c.edits == 0
                && (c.trusted && c.freq >= EXACT_PROTECT_MIN_FREQ
                    || c.sentence && c.score + EXACT_SENTENCE_MARGIN >= leader)
        })
        .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
        .map(|(i, _)| i)
    {
        cands[..=index].rotate_right(1);
    }
}
