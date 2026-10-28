//! 候选生成:原始输入上的音节图 + 词图 Viterbi 解码。
//!
//! 词图按**原始字节位置**建:同一串输入的所有切分天然并存,不再枚举切分路径,
//! 也不会因为路径数上限漏掉正确切分。整句分 = Σ 词节点分 + Σ 搭配分。
//!
//! 词节点分把词频归一成对数概率(`ln(freq/total)`),再减每词固定罚:拆成更多
//! 高频碎片的路径要额外付出每词一份的归一化代价。搭配分来自 rime `.gram`
//! 模型(可缺省),按 librime-octagram 的搭配/未搭配/句末罚参与整句排序。
//!
//! 文件布局:`query_querier` 放查询缓存,`query_builder` 放词图枚举与解码。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use lueying_dict::Dictionary;
use lueying_dict::gram::{CONTEXT_CHARS, Gram};
use lueying_dict::syllables::{self, Fuzzy};

use crate::segment::{self, Syll};
use crate::typo;
use crate::user::UserDict;

#[path = "query_builder.rs"]
mod builder;
#[path = "query_querier.rs"]
mod querier;

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
    /// 用户词库在该键下精确学过这个词(全拼与简拼互认),可作为首位覆盖的依据。
    pub learned: bool,
    /// 命中的模糊组位掩码(0 表示精确)。
    pub groups: u8,
    /// 模糊候选的注音提示(如 `shi`);精确候选为空。
    pub hint: Option<String>,
    /// 纠正候选的学习键(原串与纠正串不同,不能从 `spans` 反推)。
    pub learn_key: Option<String>,
    /// 快捷输入(日期/时间/数字金额/符号):不写词库,也不参与词级转移。
    pub quick: bool,
    /// 用户在同键下跳过过该候选:只小幅降权,精确首位保护也会让位。
    pub rejected: bool,
    /// 显示文本是繁体时的简体规范形式。学习与左邻上下文用它,避免简繁分裂词库。
    pub learn_text: Option<String>,
    /// 表情、英文直通等固定候选:不学习、不造词,也不参与模型重排。
    pub no_learn: bool,
    /// 末音节补全候选:吃掉整串输入,学习键是补全后的音节串。
    pub completion: bool,
    /// 用户在当前键下置顶的词:直接放首位,重排与快捷项都让位。
    pub pinned: bool,
}

impl Candidate {
    /// 功能模块添加“整串上屏”候选的最小构造:一个区间覆盖整个原始输入,
    /// 上屏后组合结束。`quick` 保持 false,由模块按需改写。
    pub fn whole(raw_len: usize, text: impl Into<String>, score: f64) -> Self {
        let text = text.into();
        Self {
            words: vec![text.clone()],
            text,
            consume: 1,
            score,
            spans: vec![(0, raw_len)],
            full: true,
            trusted: false,
            sentence: false,
            edits: 0,
            freq: 0,
            user_freq: 0,
            learned: false,
            groups: 0,
            hint: None,
            learn_key: None,
            quick: false,
            rejected: false,
            learn_text: None,
            no_learn: false,
            completion: false,
            pinned: false,
        }
    }

    /// 覆盖整个输入的固定快捷候选:不学习、不参与重排,顺序由插入位置决定。
    pub fn quick(raw_len: usize, text: impl Into<String>, score: f64) -> Self {
        let mut cand = Self::whole(raw_len, text, score);
        cand.words.clear();
        cand.quick = true;
        cand
    }
}

/// 一次构建的产物。
pub struct Built {
    pub cands: Vec<Candidate>,
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
/// 完整拼音输入里混入简拼单元的额外罚分。`hen` 切成 `he` + `n` 能把「很亮」
/// 读成「和能量」,这类切分在用户打全拼时不是本意;每个简拼单元扣 8 分,
/// 足以让完整音节读法胜出,同时不影响真正的简拼输入(整串无法按全拼切完)。
const ABBREV_PENALTY: f64 = 8.0;
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
/// 3.0 会让「藏书/参数」这类词频差 20–50 倍的精确整句落在门外,放宽到 5.0;
/// 口音语料与中立难例一起看,若保护了「总过」这类勉强切分就回收。
const EXACT_SENTENCE_MARGIN: f64 = 5.0;
/// 签名翻转变体上限;超过后只保留原键与单点翻转,保证至少有一条召回。
const MAX_SIG_VARIANTS: usize = 32;
/// 全拼模糊组合的键数上限;按改动处数截断,原键与单点模糊优先保留。
const MAX_FUZZY_KEYS: usize = 64;
/// 超过这个字节数按长句处理。长句每个词都展开 64 种模糊组合时,索引查询
/// 随音节数放大,首键延迟会被拖到几十毫秒;保留原键与单点模糊已够用。
const LONG_INPUT_BYTES: usize = 30;
/// 长句的模糊键上限:原键 + 各位置单点变体,丢掉多点组合。
const MAX_FUZZY_KEYS_LONG: usize = 8;
/// 精确整词达到这个频次就不补全;1 表示只要词典里有整词就不猜用户少打了字母。
const COMPLETION_EXACT_MIN_FREQ: u64 = 1;
/// 补全只面向词,不面向整句。四音节以上的输入按句子处理,避免长句每次
/// 按键都去枚举末音节,把整句候选挤出首页。
const COMPLETION_MAX_SYLLABLES: usize = 4;

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
    /// 长句只保留单点模糊;短词不受影响。
    long: bool,
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

/// 错音纠正的总预算。最长 24 键最多 400 条纠正,每条都要整图解码;
/// 慢机器上全部跑完会把首键拖到几百毫秒。按时间截断,常见手误在前几条
/// 就能命中,正常输入完全不受影响。
const TYPO_BUDGET: Duration = Duration::from_millis(6);

/// 一次候选构建的全部输入。字段数超过 Clippy 阈值,打包成一个结构;
/// 错音纠正只替换 `raw`,其余输入复用。
pub(crate) struct BuildArgs<'a> {
    pub dict: &'a Dictionary,
    pub user: &'a UserDict,
    pub fuzzy: &'a Fuzzy,
    pub gram: Option<&'a Gram>,
    pub cache: &'a mut HashMap<(String, String, bool), f64>,
    pub context: &'a str,
    pub host: &'a str,
    pub previous2: Option<&'a str>,
    pub previous: Option<&'a str>,
    pub raw: &'a str,
}

/// 候选生成入口:正常解码没有整句候选时,尝试一次相邻键/漏键纠正。
///
/// 纠正只在精确与模糊都出不了整句时发生,避免影响正常输入。同一手误可能对应
/// 多条可切分的纠正串(`hoa` 既能补成 `oha` 也能调换成 `hao`),这里取候选
/// 打分最高的一条,不先到先得。纠正串可能与原串长度不同,候选按整串上屏,
/// 学习键单独记录纠正后的音节串。
pub(crate) fn build(args: BuildArgs<'_>) -> Built {
    let BuildArgs {
        dict,
        user,
        fuzzy,
        gram,
        cache,
        context,
        host,
        previous2,
        previous,
        raw,
    } = args;
    let built = builder::build_exact(BuildArgs {
        dict,
        user,
        fuzzy,
        gram,
        cache: &mut *cache,
        context,
        host,
        previous2,
        previous,
        raw,
    });
    // 整句候选只有拆成多个词时才可能是「每个字母都当简拼」拼出来的假整句
    // (如 `hoa` → 好哦啊);完整拼音的整句和单词命中都按正常输入处理。
    let solid = built.cands.iter().any(|c| c.full && !c.sentence);
    let full_pinyin = segment::complete_full(raw);
    if solid
        || (full_pinyin && built.cands.iter().any(|c| c.full))
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
        let mut fixed = builder::build_exact(BuildArgs {
            dict,
            user,
            fuzzy,
            gram,
            cache: &mut *cache,
            context,
            host,
            previous2,
            previous,
            raw: &correction,
        });
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
        .filter(|(_, c)| c.learned && !c.rejected && (c.full || !has_full))
        .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
        .map(|(i, _)| i)
    {
        cands[..=index].rotate_right(1);
        return;
    }
    let leader = cands[0].score;
    let leader_is_completion = cands[0].completion;
    if let Some(index) = cands
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            !c.rejected
                && c.full
                && c.edits == 0
                && (c.trusted && c.freq >= EXACT_PROTECT_MIN_FREQ
                    || c.sentence
                        && !leader_is_completion
                        && c.score + EXACT_SENTENCE_MARGIN >= leader)
        })
        .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
        .map(|(i, _)| i)
    {
        cands[..=index].rotate_right(1);
    }
}
