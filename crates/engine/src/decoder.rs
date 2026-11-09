//! 解码:音节图上的词图 Viterbi,一次解码产出全部候选。
//!
//! 词图按**原始字节位置**建:同一串输入的所有切分与所有读法天然并存。路径分 = Σ 词的
//! 语言分 + Σ 搭配分 − Σ 读法代价;第一个词的搭配分取已上屏的文字,所以 1–3 字一段地打
//! 时,每段都接着上文解码。
//!
//! 按键是增量的:组合通常只在末尾增删一个字母,前缀位置上的束与词节点不变。[`Charts`]
//! 保留上一次解码的各位置列,只重算变化的尾部。复用的条件是前缀字节、末尾可完成性与
//! 整串级标志([`Variant`])都相同,此时结果与整串从头算逐位相等。
//!
//! 词的语言分把词频归一成对数概率,再减每词固定罚:拆成更多高频碎片的路径要额外付出
//! 每词一份的归一化代价。读法代价来自 [`crate::channel`],全拼、简拼、模糊音、手误同轴。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lueying_dict::gram::{CONTEXT_CHARS, Gram};
use lueying_dict::{Dictionary, Fuzzy, syllables};

use crate::channel::{self, Unit};
use crate::lexicon::{CHAR_BONUS, Hit, Lexicon, WORD_PENALTY, user_gain};
use crate::query::{BuildArgs, Built, Candidate};
use crate::user::UserDict;

/// 首页留给整句的名额。
const FIRST_PAGE: usize = 5;
/// 中间位置每个终点保留的不同文本路径数。
const BEAM: usize = 200;
/// 末位束宽:整句候选的上限。候选上限还要给逐段上屏的前缀词留位置。
const BEAM_LAST: usize = 300;
const MAX_CANDS: usize = BEAM_LAST + 32;
/// 每个 (起点, 终点) 保留的最高分节点数。同音字里正确的字常排在前十之外,要靠束与
/// 重排才能认出来;节点限额与束宽同时放大才有效(见 design.txt)。
const NODE_CAP: usize = 48;
/// 一个词吃掉整串输入时不需要再和别的词拼接,多留几条也不增加解码量。
const NODE_CAP_WHOLE: usize = 96;
/// 一个词最多几个音节(与词库最长词组一致)。
const MAX_WORD_SYL: usize = 8;
/// 建图访问预算:简拼混合输入的组合可能很多,超出后停止扩词。超出时结果依赖整串,
/// 这一轮不复用前缀列。
const NODE_BUDGET: usize = 4096;
/// 一个词里最多几处手误。手误通道整句全程开放后,枚举量与误救风险都随词数放大,
/// 长句仍限一处;短输入放宽到两处,真实手快常在一词里连犯两下。
const MAX_WORD_TYPOS: usize = 2;
const MAX_WORD_TYPOS_LONG: usize = 1;
/// 一个词里最多几处模糊改动。超过这个字节数按长句处理,只留单点模糊,否则索引查询
/// 随音节数放大,首键延迟会被拖到几十毫秒。
const MAX_WORD_EDITS: usize = 3;
const MAX_WORD_EDITS_LONG: usize = 1;
const LONG_INPUT_BYTES: usize = 30;
/// 连续声母里,第二个词起补偿一部分每词罚,让任意声母串都能连成整句候选。
const ABBREV_SEQUENCE_BONUS: f64 = 1.4;
/// 每处模糊改动的代价;末音节补全按少打的字母数计。
const COMPLETION_LETTER_COST: f64 = std::f64::consts::LN_2;
/// 精确首位保护的最低词频。只要词库给出非零频次,就优先保住用户实际打出的
/// 全拼整词;零频长尾仍按分数竞争,避免把无意义词条顶到首位。
const EXACT_PROTECT_MIN_FREQ: u64 = 1;
/// 整句精确保护允许的落后幅度(对数概率)。整句没有词典频次背书,只有分数
/// 与首位接近时才说明两种读法都成立;差距过大留给模糊读法。
/// 3.0 会让「藏书/参数」这类词频差 20–50 倍的精确整句落在门外,放宽到 5.0;
/// 口音语料与中立难例一起看,若保护了「总过」这类勉强切分就回收。
const EXACT_SENTENCE_MARGIN: f64 = 5.0;
/// 精确整词达到这个频次就不补全;1 表示只要词典里有整词就不猜用户少打了字母。
const COMPLETION_EXACT_MIN_FREQ: u64 = 1;
/// 补全词频超过精确词这么多倍时,精确词只是语料碎片,不能挡住补全:
/// `niha` 的「你哈」不该挡住「你好」,`nanshen` 的「男神」仍挡住「男生」。
const COMPLETION_OVERRIDE_RATIO: u64 = 16;
/// 补全只面向词,不面向整句。四音节以上的输入按句子处理,避免长句每次
/// 按键都去枚举末音节,把整句候选挤出首页。
const COMPLETION_MAX_SYLLABLES: usize = 4;
/// 上下文前缀图最多保留几种整串级变体(见 [`Variant`])。
const CHARTS_KEPT: usize = 4;
/// 词命中缓存的条目上限;一次输入用到的单元串远少于此。
const HITS_KEPT: usize = 1 << 14;

/// 词图节点:一个词覆盖 `[start, end)` 这段原始输入。
struct Node {
    start: usize,
    word: Arc<str>,
    /// 语言分减读法代价。
    score: f64,
    /// 读法代价(已扣模糊组学习的减免)。
    channel: f64,
    /// `score` 里来自用户选词次数的部分。
    personal: f64,
    reading: Arc<Reading>,
    /// 零代价读法的词典命中;简拼也算,全拼串里混入的简拼有代价,不算。
    trusted: bool,
    edits: usize,
    freq: u64,
    user_freq: u32,
    /// 用户在这组音节上直接学过这个词。
    learned: bool,
    groups: u8,
    /// 模糊候选的词条键,展示时做注音提示。
    accent: Option<Arc<str>>,
    corrected: bool,
}

/// 同一串单元读出的词共用这份区间与学习键。
struct Reading {
    spans: Vec<(usize, usize)>,
    /// 手误音节取纠正后的音节,其余取原文。
    keys: String,
}

/// 词枚举的游标状态。`finish[p]` 表示 `raw[p..]` 能否被音节图读完:
/// 词尾落在读不动的位置时,残留的字母永远无法上屏。
struct Walker<'a, 'd> {
    lexicon: &'a mut Lexicon<'d>,
    user: &'d UserDict,
    edges: &'a [Vec<Unit>],
    raw: &'a str,
    finish: &'a [bool],
    /// 只保留终点不小于它的词;更靠前的列已经在前缀图里。
    first_end: usize,
    found: &'a mut [Vec<Node>],
    units: Vec<&'a Unit>,
    visits: usize,
    /// 访问预算耗尽过:结果依赖整串,不能复用前缀列。
    truncated: bool,
    max_edits: usize,
    max_typos: usize,
    /// 每串单元最多建几个节点。词表已按语言分排好,排在后面的进不了束。
    cap: usize,
}

impl Walker<'_, '_> {
    /// 从 `pos` 起深度优先枚举词,写到 `found[词尾]`。
    fn run(&mut self, root: usize, pos: usize) {
        if !self.units.is_empty() {
            if !self.finish[pos] {
                return;
            }
            self.emit(root, pos);
            self.visits += 1;
            if self.units.len() >= MAX_WORD_SYL || !self.lexicon.may_extend(&self.units) {
                return;
            }
            if self.visits > NODE_BUDGET {
                self.truncated = true;
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
        let edges = self.edges;
        for unit in &edges[pos] {
            if !self.admits(unit) {
                continue;
            }
            self.units.push(unit);
            self.run(root, unit.span.1);
            self.units.pop();
        }
    }

    fn admits(&self, unit: &Unit) -> bool {
        let typos = self.units.iter().filter(|known| known.typo).count() + usize::from(unit.typo);
        let edits = self.units.iter().map(|known| known.edits).sum::<usize>() + unit.edits;
        // 简拼本身已丢掉大半读音,再叠手误或多处模糊就只是在碰运气,还最费查询。
        let abbrev = unit.abbrev || self.units.iter().any(|known| known.abbrev);
        if abbrev {
            typos == 0 && edits <= MAX_WORD_EDITS_LONG
        } else {
            typos <= self.max_typos && edits <= self.max_edits
        }
    }

    fn emit(&mut self, root: usize, end: usize) {
        if end < self.first_end {
            return;
        }
        let hits = self.lexicon.lookup(&self.units);
        if hits.is_empty() {
            return;
        }
        let cost: f64 = self.units.iter().map(|unit| unit.cost).sum();
        let edits: usize = self.units.iter().map(|unit| unit.edits).sum();
        let typos = self.units.iter().filter(|unit| unit.typo).count();
        let groups = self.units.iter().fold(0, |mask, unit| mask | unit.groups);
        // 组级学习只降低该组的模糊代价,不改精确命中的保护。
        let relief = if groups == 0 {
            0.0
        } else {
            self.user.group_bonus(groups)
        };
        let keys = self
            .units
            .iter()
            .map(|unit| {
                if unit.typo {
                    unit.text.as_str()
                } else {
                    syllables::canonical(&self.raw[unit.span.0..unit.span.1])
                }
            })
            .collect::<Vec<_>>()
            .join("'");
        let reading = Arc::new(Reading {
            spans: self.units.iter().map(|unit| unit.span).collect(),
            keys,
        });
        let limit = if root == 0 && end == self.raw.len() {
            NODE_CAP_WHOLE
        } else {
            self.cap
        };
        for hit in hits.iter().take(limit) {
            self.found[end].push(Node {
                start: root,
                word: Arc::clone(&hit.word),
                score: hit.prior - cost + relief,
                channel: (cost - relief).max(0.0),
                personal: user_gain(hit.user_freq),
                reading: Arc::clone(&reading),
                trusted: hit.dictionary && cost == 0.0,
                edits: edits + typos,
                freq: hit.freq,
                user_freq: hit.user_freq,
                learned: hit.learned && typos == 0,
                groups,
                accent: (edits > 0).then(|| Arc::clone(&hit.key)),
                corrected: typos > 0,
            });
        }
    }
}

/// 枚举 `first_end` 及之后终点的全部词节点。返回按终点分组的节点,以及访问预算是否耗尽过。
fn enumerate<'d>(
    lexicon: &mut Lexicon<'d>,
    user: &'d UserDict,
    edges: &[Vec<Unit>],
    raw: &str,
    finish: &[bool],
    variant: Variant,
    first_end: usize,
) -> (Vec<Vec<Node>>, bool) {
    let mut found: Vec<Vec<Node>> = std::iter::repeat_with(Vec::new)
        .take(raw.len() + 1)
        .collect();
    let mut walker = Walker {
        lexicon,
        user,
        edges,
        raw,
        finish,
        first_end,
        found: &mut found,
        units: Vec::new(),
        visits: 0,
        truncated: false,
        max_edits: if variant.long {
            MAX_WORD_EDITS_LONG
        } else {
            MAX_WORD_EDITS
        },
        max_typos: if variant.long {
            MAX_WORD_TYPOS_LONG
        } else {
            MAX_WORD_TYPOS
        },
        cap: NODE_CAP,
    };
    for (start, &ok) in finish.iter().take(raw.len()).enumerate() {
        if raw.as_bytes()[start] == b'\'' || !ok {
            continue;
        }
        walker.units.clear();
        walker.visits = 0;
        walker.run(start, start);
    }
    let truncated = walker.truncated;
    (found, truncated)
}

/// 每个终点上按起点分组,去重后保留每组最高分的若干节点。
fn cap_nodes(mut list: Vec<Node>, whole_end: bool, cap: usize) -> Vec<Arc<Node>> {
    list.sort_by(|a, b| a.start.cmp(&b.start).then(b.score.total_cmp(&a.score)));
    let mut at: HashMap<(usize, Arc<str>), usize> = HashMap::new();
    let mut counts: HashMap<usize, usize> = HashMap::new();
    let mut kept: Vec<Node> = Vec::new();
    for node in list {
        let id = (node.start, Arc::clone(&node.word));
        if let Some(&index) = at.get(&id) {
            // 用户词盖过了同一个词的模糊读法:选中它仍算这个模糊组的一次使用。
            if kept[index].freq == 0 {
                kept[index].groups |= node.groups;
            }
            continue;
        }
        let limit = if whole_end && node.start == 0 {
            NODE_CAP_WHOLE
        } else {
            cap
        };
        let count = counts.entry(node.start).or_default();
        if *count < limit {
            *count += 1;
            at.insert(id, kept.len());
            kept.push(node);
        }
    }
    kept.into_iter().map(Arc::new).collect()
}

/// 解码路径,共享前缀。
struct Link {
    node: Arc<Node>,
    prev: Option<Arc<Link>>,
}

#[derive(Clone)]
struct State {
    score: f64,
    channel: f64,
    personal: f64,
    text: String,
    link: Option<Arc<Link>>,
    /// 整条路径由单个精确词典词构成时,才允许让模型让位。
    trusted: bool,
    /// 单节点路径的词典词频;多词拼句为 0。
    freq: u64,
    user_freq: u32,
    learned: bool,
    /// 路径累计的模糊改动与手误处数。
    edits: usize,
    /// 路径累计的模糊组。
    groups: u8,
    corrected: bool,
}

impl State {
    fn start() -> Self {
        Self {
            score: 0.0,
            channel: 0.0,
            personal: 0.0,
            text: String::new(),
            link: None,
            trusted: true,
            freq: 0,
            user_freq: 0,
            learned: false,
            edits: 0,
            groups: 0,
            corrected: false,
        }
    }

    fn extend(&self, node: &Arc<Node>, gain: f64) -> Self {
        let first = self.link.is_none();
        let mut text = String::with_capacity(self.text.len() + node.word.len());
        text.push_str(&self.text);
        text.push_str(&node.word);
        Self {
            score: self.score + node.score + gain,
            channel: self.channel + node.channel,
            personal: self.personal + node.personal,
            text,
            link: Some(Arc::new(Link {
                node: Arc::clone(node),
                prev: self.link.clone(),
            })),
            trusted: first && node.trusted,
            freq: if first { node.freq } else { 0 },
            user_freq: if first { node.user_freq } else { 0 },
            learned: first && node.learned,
            edits: self.edits + node.edits,
            groups: self.groups | node.groups,
            corrected: self.corrected || node.corrected,
        }
    }

    /// 路径上的节点,从左到右。
    fn nodes(&self) -> Vec<Arc<Node>> {
        let mut out = Vec::new();
        let mut at = self.link.as_ref();
        while let Some(link) = at {
            out.push(Arc::clone(&link.node));
            at = link.prev.as_ref();
        }
        out.reverse();
        out
    }
}

/// 从按分数降序排好的路径里取前 `width` 条不同文本。
///
/// 往后的搭配分只看文本尾部,同位置同文本的路径分差不会再变,低分的那条永远赢不了,
/// 留着只是把别的读法挤出束。信任与词频取并集,和整句候选的去重口径一致。
fn distinct_beam(sorted: impl Iterator<Item = State>, width: usize) -> Vec<State> {
    let mut at: HashMap<String, usize> = HashMap::with_capacity(width);
    let mut kept: Vec<State> = Vec::with_capacity(width);
    for state in sorted {
        if let Some(&index) = at.get(&state.text) {
            let first = &mut kept[index];
            first.trusted |= state.trusted;
            first.learned |= state.learned;
            first.freq = first.freq.max(state.freq);
            first.user_freq = first.user_freq.max(state.user_freq);
            continue;
        }
        at.insert(state.text.clone(), kept.len());
        kept.push(state);
        if kept.len() == width {
            break;
        }
    }
    kept
}

/// 上屏文字接解码文本的尾部,最多 `CONTEXT_CHARS` 个字。
fn tail(context: &str, text: &str) -> String {
    let mut chars: Vec<char> = text.chars().rev().take(CONTEXT_CHARS).collect();
    let missing = CONTEXT_CHARS - chars.len();
    chars.extend(context.chars().rev().take(missing));
    chars.into_iter().rev().collect()
}

fn hint_of(corrected: bool, key: &str, accent: Option<&str>) -> Option<String> {
    if corrected {
        Some(format!("纠自 {}", key.replace('\'', " ")))
    } else {
        accent.map(|key| key.replace('\'', " "))
    }
}

/// 会改变前缀列数值的整串级标志。标志变了,同一组列就不能再复用。
#[derive(Clone, Copy, PartialEq, Eq)]
struct Variant {
    /// 整串能全拼读完:简拼单元的代价与手误单元的额外代价随之变化。
    complete: bool,
    /// 手误通道开着(快捷触发键时关闭)。
    typos: bool,
    /// 整串是纯声母缩写序列:相邻词多给一份连接分。
    abbrev_sequence: bool,
    /// 长输入:每词的模糊与手误上限不同。
    long: bool,
}

impl Variant {
    fn of(raw: &str, quick_trigger: bool) -> Self {
        Self {
            complete: channel::is_complete(raw),
            typos: !quick_trigger,
            abbrev_sequence: channel::is_abbrev_sequence(raw),
            long: raw.len() > LONG_INPUT_BYTES,
        }
    }
}

/// 一个终点位置的列:以它为终点的词节点,以及接上这些节点的束。
struct Column {
    nodes: Vec<Arc<Node>>,
    beam: Vec<State>,
    /// `beam` 中出现过的搭配上文(尾部 `CONTEXT_CHARS` 字),各只一条。搭配分只看上文,
    /// 同上文的路径共享一次查询。
    tails: Vec<String>,
    /// 每条路径的上文在 `tails` 中的下标。
    tail_of: Vec<usize>,
}

impl Column {
    fn new(nodes: Vec<Arc<Node>>, beam: Vec<State>, context: &str) -> Self {
        let mut tails: Vec<String> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut tail_of = Vec::with_capacity(beam.len());
        for state in &beam {
            let text = tail(context, &state.text);
            let at = *index.entry(text.clone()).or_insert_with(|| {
                tails.push(text);
                tails.len() - 1
            });
            tail_of.push(at);
        }
        Self {
            nodes,
            beam,
            tails,
            tail_of,
        }
    }

    /// 位置 0:只有一条空路径。
    fn start(context: &str) -> Self {
        Self::new(Vec::new(), vec![State::start()], context)
    }
}

/// 一次解码内不变的输入:整串、上文与缩写序列标志。
struct Input<'a> {
    raw: &'a str,
    context: &'a str,
    abbrev_sequence: bool,
}

/// 候选对:路径分、路径,以及可选的接续(词节点与搭配分)。
type Pair<'a> = (f64, &'a State, Option<(&'a Arc<Node>, f64)>);

/// 以 `end` 为终点的一列:由以前各列的束接上以 `end` 结束的词节点。
fn build_column(
    end: usize,
    nodes: Vec<Arc<Node>>,
    columns: &[Column],
    input: &Input<'_>,
    width: usize,
    rear: bool,
    gram: Option<&Gram>,
) -> Column {
    let (raw, context) = (input.raw, input.context);
    if end == 0 {
        return Column::start(context);
    }
    // 隔音符不占音节:让上一位置的状态直接越过它,词与词才能在 `'` 两侧相接。
    let mut next: Vec<Pair<'_>> = Vec::new();
    if raw.as_bytes()[end - 1] == b'\'' {
        next.extend(
            columns[end - 1]
                .beam
                .iter()
                .map(|state| (state.score, state, None)),
        );
    }
    for node in &nodes {
        let prev = &columns[node.start];
        let bonus: Vec<f64> = prev
            .tails
            .iter()
            .map(|before| gram.map_or(0.0, |gram| gram.score(before, &node.word, rear)))
            .collect();
        for (state, &at) in prev.beam.iter().zip(&prev.tail_of) {
            let sequence = if input.abbrev_sequence && state.link.is_some() {
                ABBREV_SEQUENCE_BONUS
            } else {
                0.0
            };
            let gain = sequence + bonus[at];
            next.push((state.score + node.score + gain, state, Some((node, gain))));
        }
    }
    let beam = top_beam(next, width);
    Column::new(nodes, beam, context)
}

/// 按分数取前 `width` 条不同文本的束。只把分数最高的一截排序:截断不够时加倍再取。
fn top_beam(mut next: Vec<Pair<'_>>, width: usize) -> Vec<State> {
    let by_score =
        |a: &(f64, &State, Option<(&Arc<Node>, f64)>),
         b: &(f64, &State, Option<(&Arc<Node>, f64)>)| { b.0.total_cmp(&a.0) };
    let mut k = (2 * width).min(next.len());
    loop {
        let head = if k < next.len() {
            next.select_nth_unstable_by(k, by_score);
            &mut next[..k]
        } else {
            &mut next[..]
        };
        head.sort_by(by_score);
        let beam = distinct_beam(
            head.iter().map(|(_, state, step)| match step {
                Some((node, gain)) => state.extend(node, *gain),
                None => (*state).clone(),
            }),
            width,
        );
        if beam.len() == width || k == next.len() {
            return beam;
        }
        k = (2 * k).min(next.len());
    }
}

/// 上一次解码的前缀图:一组变体下每个内部位置的列,连同它所对应的整串。
pub(crate) struct Chart {
    variant: Variant,
    raw: String,
    /// `raw` 各位置的可完成性。
    finish: Vec<bool>,
    /// 上一次解码的访问预算是否耗尽过;耗尽时整串的列都不可复用。
    truncated: bool,
    /// 位置 `0..raw.len()` 的列(末位不在其中,它每次都要重算)。
    columns: Vec<Column>,
}

impl Chart {
    fn new(variant: Variant) -> Self {
        Self {
            variant,
            raw: String::new(),
            finish: Vec::new(),
            truncated: false,
            columns: Vec::new(),
        }
    }

    /// 新整串能复用本图的前几列:前缀字节相同、末尾可完成性相同,且列是内部列。
    fn reusable(&self, raw: &str, finish: &[bool]) -> usize {
        if self.truncated {
            return 0;
        }
        let shared = raw
            .bytes()
            .zip(self.raw.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let mut reuse = 0;
        while reuse < self.columns.len()
            && reuse < raw.len()
            && reuse <= shared
            && self.finish[reuse] == finish[reuse]
        {
            reuse += 1;
        }
        reuse
    }
}

/// 解码之间保留的前缀图。上下文、用户词库修订或模糊配置变了,整体作废。
#[derive(Default)]
pub(crate) struct Charts {
    context: String,
    user: u64,
    fuzzy: Option<Fuzzy>,
    charts: Vec<Chart>,
    /// 词命中缓存,与前缀图同一失效条件(见 [`Lexicon`])。
    hits: HashMap<String, Arc<Vec<Hit>>>,
}

impl Charts {
    /// 上文、用户词库修订或模糊配置变了,前缀图与词命中缓存整体作废。
    fn sync(&mut self, context: &str, user: u64, fuzzy: Fuzzy) {
        // 上文一直不变时(私密输入、宿主不给上文)命中缓存只增不减,超过上限就丢掉重填。
        if self.hits.len() > HITS_KEPT {
            self.hits.clear();
        }
        if self.user != user || self.fuzzy != Some(fuzzy) || self.context != context {
            self.charts.clear();
            self.hits.clear();
            self.context.clear();
            self.context.push_str(context);
            self.user = user;
            self.fuzzy = Some(fuzzy);
        }
    }

    fn chart(&mut self, variant: Variant) -> &mut Chart {
        let index = match self
            .charts
            .iter()
            .position(|chart| chart.variant == variant)
        {
            Some(index) => index,
            None => {
                if self.charts.len() == CHARTS_KEPT {
                    self.charts.remove(0);
                }
                self.charts.push(Chart::new(variant));
                self.charts.len() - 1
            }
        };
        &mut self.charts[index]
    }
}

/// 整串各位置的可完成性:`raw[p..]` 能否被音节图读完。隔音符位置与其后相同。
fn finishable(raw: &str, edges: &[Vec<Unit>]) -> Vec<bool> {
    let mut finish = vec![false; raw.len() + 1];
    finish[raw.len()] = true;
    for i in (0..raw.len()).rev() {
        finish[i] = if raw.as_bytes()[i] == b'\'' {
            finish[i + 1]
        } else {
            edges[i].iter().any(|unit| finish[unit.span.1])
        };
    }
    finish
}

pub(crate) fn decode(args: BuildArgs<'_>) -> Built {
    let BuildArgs {
        dict,
        user,
        fuzzy,
        gram,
        context,
        host,
        previous2,
        previous,
        raw,
        quick_trigger,
        charts,
    } = args;
    let total = dict.total_weight() as f64;
    let left = crate::user::left_key(context);
    charts.sync(context, user.revision(), *fuzzy);
    let mut lexicon = Lexicon::new(
        dict,
        user,
        total,
        (!context.is_empty()).then_some(left),
        std::mem::take(&mut charts.hits),
    );
    let edges = channel::lattice(raw, fuzzy, !quick_trigger);
    let finish = finishable(raw, &edges);
    let variant = Variant::of(raw, quick_trigger);
    let mut reuse = charts.chart(variant).reusable(raw, &finish);
    let (mut found, mut truncated) =
        enumerate(&mut lexicon, user, &edges, raw, &finish, variant, reuse);
    if truncated && reuse > 0 {
        // 预算耗尽时整串结果依赖全部词节点:整串重算,与不复用前缀的结果一致。
        reuse = 0;
        (found, truncated) = enumerate(&mut lexicon, user, &edges, raw, &finish, variant, 0);
    }
    charts.hits = lexicon.into_hits();
    let chart = charts.chart(variant);

    let n = raw.len();
    let abbrev_sequence = variant.abbrev_sequence;
    let mut columns = std::mem::take(&mut chart.columns);
    columns.truncate(reuse);
    if columns.is_empty() {
        columns.push(Column::start(context));
    }
    let input = Input {
        raw,
        context,
        abbrev_sequence,
    };
    for (end, slot) in found.iter_mut().enumerate().take(n).skip(columns.len()) {
        let nodes = cap_nodes(std::mem::take(slot), false, NODE_CAP);
        let column = build_column(end, nodes, &columns, &input, BEAM, false, gram);
        columns.push(column);
    }
    let last_nodes = cap_nodes(std::mem::take(&mut found[n]), true, NODE_CAP);
    let last = build_column(n, last_nodes, &columns, &input, BEAM_LAST, true, gram);

    let mut full: Vec<Candidate> = Vec::new();
    for state in &last.beam {
        if state.text.is_empty() {
            continue;
        }
        let nodes = state.nodes();
        let spans: Vec<(usize, usize)> = nodes
            .iter()
            .flat_map(|node| node.reading.spans.iter().copied())
            .collect();
        let key = nodes
            .iter()
            .map(|node| node.reading.keys.as_str())
            .collect::<Vec<_>>()
            .join("'");
        let reject = user.reject_penalty_in(left, &key, &state.text);
        let fuzzy_hint = match nodes.as_slice() {
            [only] => only.accent.as_deref(),
            _ => None,
        };
        let mut cand = Candidate::whole(raw.len(), state.text.clone(), state.score - reject);
        cand.words = nodes.iter().map(|node| node.word.to_string()).collect();
        cand.consume = spans.len();
        cand.spans = spans;
        cand.channel = state.channel;
        cand.personal = state.personal - reject;
        cand.trusted = state.trusted;
        cand.sentence = nodes.len() > 1 && state.edits == 0;
        cand.edits = state.edits;
        cand.freq = state.freq;
        cand.user_freq = state.user_freq;
        cand.learned = state.learned;
        cand.groups = state.groups;
        cand.hint = hint_of(state.corrected, &key, fuzzy_hint);
        cand.learn_key = Some(key);
        cand.correction = state.corrected;
        cand.rejected = reject > 0.0;
        full.push(cand);
    }
    // 个人上下文只调整整句候选的顺序:词频、读法代价和搭配分已经决定了进入束的路径,
    // 这里再让用户常打的前后搭配与词序在同一批路径里靠前。
    let personal = |cand: &mut Candidate| {
        let gain = user.context_bonus(context, &cand.text)
            + user.app_bonus(host, &cand.words)
            + user.transition_bonus(previous, &cand.words)
            + user.trigram_bonus(previous2, previous, &cand.words);
        cand.score += gain;
        cand.personal += gain;
    };
    full.iter_mut().for_each(personal);
    // 把末音节补成更长的音节。像 `bulianhougu` 这种少打末尾字母的输入,精确图
    // 只能给出碎词;补全让整词也有机会进首页。有精确词时补全要比它常用得多才算。
    let exact = full
        .iter()
        .filter(|c| exact_full_word(raw, c))
        .map(|c| c.freq)
        .max()
        .unwrap_or(0);
    let floor = exact.saturating_mul(COMPLETION_OVERRIDE_RATIO);
    let completed = completion_candidates(raw, dict, total);
    full.extend(completed.into_iter().filter(|c| c.freq >= floor));
    full.sort_by(|a, b| b.score.total_cmp(&a.score));
    // 同一句话可能由多种切分拼出,上屏文本完全相同;先按文本去重再占首页
    // 名额,否则重复整句会把前缀词挤进第一页,逐段上屏入口的位置忽前忽后。
    // 文本相同即用户可见结果相同,信任与学习标记取并集,别让去重丢掉整词证据。
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut unique: Vec<Candidate> = Vec::with_capacity(full.len());
    for cand in full {
        if let Some(&index) = at.get(&cand.text) {
            let kept = &mut unique[index];
            kept.trusted |= cand.trusted;
            kept.learned |= cand.learned;
            // 手误读法碰巧读出同一个词,不说明用户把末音节打完了。
            kept.completion &= cand.completion || cand.correction;
            kept.freq = kept.freq.max(cand.freq);
            kept.user_freq = kept.user_freq.max(cand.user_freq);
            continue;
        }
        at.insert(cand.text.clone(), unique.len());
        unique.push(cand);
    }
    // 整串本身读得通时,纠错只作后备:首页先放字面读法,也不占分段入口的位置。
    let complete = channel::is_complete(raw);
    let mut full: Vec<Candidate> = Vec::with_capacity(unique.len());
    let mut fallback: Vec<Candidate> = Vec::new();
    for cand in unique {
        if complete && cand.correction && full.len() < FIRST_PAGE {
            fallback.push(cand);
        } else {
            full.push(cand);
        }
    }
    let at = full.len().min(FIRST_PAGE);
    let page = if at == 0 && !fallback.is_empty() {
        1
    } else {
        at
    };
    let later = full.split_off(at);
    full.extend(fallback);
    full.extend(later);
    // 单词候选:只取从输入开头起的词(选中后剩余输入继续组合)。
    let mut words: Vec<Candidate> = Vec::new();
    let before = tail(context, "");
    for (end, column) in columns.iter().chain(std::iter::once(&last)).enumerate() {
        for node in column.nodes.iter().filter(|node| node.start == 0) {
            let keys = node.reading.keys.as_str();
            let reject = user.reject_penalty_in(left, keys, &node.word);
            let gain = gram.map_or(0.0, |gram| {
                gram.score(&before, &node.word, end == raw.len())
            });
            let mut cand =
                Candidate::whole(raw.len(), node.word.to_string(), node.score + gain - reject);
            cand.consume = node.reading.spans.len();
            cand.spans = node.reading.spans.clone();
            cand.channel = node.channel;
            cand.personal = node.personal - reject;
            cand.full = end == raw.len();
            cand.trusted = node.trusted;
            cand.edits = node.edits;
            cand.freq = node.freq;
            cand.user_freq = node.user_freq;
            cand.learned = node.learned;
            cand.groups = node.groups;
            cand.hint = hint_of(node.corrected, keys, node.accent.as_deref());
            cand.learn_key = Some(keys.to_string());
            cand.correction = node.corrected;
            cand.rejected = reject > 0.0;
            words.push(cand);
        }
    }
    // 前缀词也吃同一份个人上下文,逐段上屏时第一段就能按习惯排序。
    words.iter_mut().for_each(personal);
    words.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            // 同一个词既能按完整音节也能按简拼命中时,优先吃满音节:
            // `haoyi` 里的「好」应上屏 hao,不能只吃掉 h 留下 aoyi。
            .then(
                b.spans
                    .last()
                    .map_or(0, |s| s.1)
                    .cmp(&a.spans.last().map_or(0, |s| s.1)),
            )
            .then(a.text.cmp(&b.text))
    });
    // 首页优先放满整句备选:长句的正确读法常排在第 6-8 条,第二位的分段
    // 前缀会把它挤出页外。五条名额先给整句,最高分前缀词顺延到第 6 位;
    // 需要逐段上屏时下移一格即可,整句不足五条时前缀词自动补位。
    let mut cands: Vec<Candidate> = Vec::new();
    let mut rest = full.into_iter();
    cands.extend(rest.by_ref().take(page));
    let taken: HashSet<String> = cands.iter().map(|c| c.text.clone()).collect();
    let mut words = words.into_iter();
    // 第 6 位的分段入口不能与已收录的整句重复;重复项留着也会被去重掉。
    if let Some(prefix) = words.find(|w| !taken.contains(&w.text)) {
        cands.push(prefix);
    }
    cands.extend(rest);
    cands.extend(words);
    // 同一文本、同样吃完当前输入的候选对用户完全等价:上屏文字与剩余输入
    // 都相同,只是切分方式不同。保留分数最高的一条;吃完长度不同的前缀词
    // 仍然各自保留(例如「今天」既可能是整句也可能只吃掉前半段)。
    let mut seen: HashSet<(String, usize)> = HashSet::new();
    cands.retain(|c| {
        let cut = c.spans.last().map_or(0, |s| s.1);
        seen.insert((c.text.clone(), cut))
    });
    protect_exact(&mut cands);
    cands.truncate(MAX_CANDS);

    chart.raw = raw.to_string();
    chart.finish = finish;
    chart.truncated = truncated;
    chart.columns = columns;
    Built { cands }
}

/// 整串精确整词:每个音节都必须是完整拼音,不能靠把半个音节当简拼凑出来。
/// `niha` 的「你好啊」来自 `ni h a`(`h` 当 `hao` 的简拼),这不是用户少打一个
/// 字母的证据;它不该挡住补全出的「你好」。
fn exact_full_word(raw: &str, cand: &Candidate) -> bool {
    cand.trusted
        && cand.freq >= COMPLETION_EXACT_MIN_FREQ
        && cand
            .spans
            .iter()
            .all(|&(start, end)| syllables::is_syllable(syllables::canonical(&raw[start..end])))
}

/// 末音节补全:把最后一个音节换成词典里更长的音节,只保留音节数不变、
/// 整词长度对得上的词条。没有精确整词时才调用,避免 `ni` 被补成 `nin/ning`
/// 这类无关候选挤进首页。
fn completion_candidates(raw: &str, dict: &Dictionary, total: f64) -> Vec<Candidate> {
    // 按最长音节切分输入,得到用户实际打出的音节序列。候选图里的切分可能
    // 来自模糊纠正,不能拿来当补全的基准。
    let spans = channel::first_path(raw);
    // 单音节输入不补全:用户可能只是打了一个完整的单字,`ni` 不该冒出 `nin/ning`。
    // 四音节以上按整句处理,补全会和整句候选抢首页,也拖慢每次按键。
    if spans.len() < 2 || spans.len() > COMPLETION_MAX_SYLLABLES {
        return Vec::new();
    }
    let typed: Vec<&str> = spans
        .iter()
        .map(|&(start, end)| syllables::canonical(&raw[start..end]))
        .collect();
    // 只补完整拼音。用户用简拼(`nih`)时补全会把 `h` 展开成几百个音节,
    // 首页灌满同前缀的猜测;简拼继续走原有整词路径。
    if !typed
        .iter()
        .all(|syllable| syllables::is_syllable(syllable))
    {
        return Vec::new();
    }
    let Some(last) = typed.last().copied() else {
        return Vec::new();
    };
    let prefix = typed[..typed.len() - 1].join("'");
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    // 直接枚举以末音节为前缀的合法音节,再按精确键取词。比扫整段前缀索引
    // 更稳:`ni'h` 的 64 条扫描很可能还没走到 `ni'hao`。
    for syllable in syllables::SYLLABLES {
        let syllable = *syllable;
        if syllable.len() <= last.len() || !syllable.starts_with(last) {
            continue;
        }
        let completed = syllables::canonical(syllable);
        if completed == last {
            continue;
        }
        let key = format!("{prefix}'{completed}");
        for entry in dict.lookup_exact(&key) {
            if entry.word.chars().count() != typed.len() || !seen.insert(entry.word.to_string()) {
                continue;
            }
            let cost = COMPLETION_LETTER_COST * (completed.len() - last.len()) as f64;
            let score = (entry.freq.max(1) as f64 / total).ln()
                + CHAR_BONUS * entry.word.chars().count() as f64
                - WORD_PENALTY
                - cost;
            let mut cand = Candidate::whole(raw.len(), entry.word, score);
            cand.channel = cost;
            cand.freq = entry.freq;
            cand.hint = Some(format!("补全 {}", entry.key.replace('\'', " ")));
            cand.learn_key = Some(entry.key.to_string());
            cand.completion = true;
            out.push(cand);
        }
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(MAX_CANDS);
    out
}

/// 排序后的最后一道闸:没打错音时,常用精确整词固定首位。
///
/// 只锁首位,不锁整页。青简把全部精确候选压在模糊前,模糊可能永远翻不到;
/// 完全按分数排,80 倍词频差又会让高频模糊词压过精确词。这里取中间:
/// 精确常用词占第 1 位,模糊候选从第 2 位开始按分数竞争。
///
/// 学习覆盖优先于精确保护:同一按键下稳定用过的词直接回到首位,与微软
/// 拼音、微信一致;词典外的新词第一次只进候选后段,连续用两三次才轮到
/// 首位。选错了可以 Shift+Delete 忘掉。模糊词同样适用,用户既然选了它,
/// 就说明这个音的分歧对他没有意义。
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
        .filter(|(_, c)| c.learned && c.user_freq >= 2 && !c.rejected && (c.full || !has_full))
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
                && (c.trusted && !leader_is_completion && c.freq >= EXACT_PROTECT_MIN_FREQ
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state(text: &str, score: f64) -> State {
        State {
            score,
            text: text.to_string(),
            trusted: false,
            ..State::start()
        }
    }

    #[test]
    fn distinct_beam_merges_segmentations_of_one_text() {
        let mut whole = state("你好", 1.0);
        whole.trusted = true;
        whole.freq = 9;
        let sorted = vec![state("你好", 2.0), whole, state("拟好", 0.5)];
        let kept = distinct_beam(sorted.into_iter(), BEAM);
        assert_eq!(kept.len(), 2);
        assert!(kept[0].trusted);
        assert_eq!(kept[0].freq, 9);
        assert_eq!(kept[1].text, "拟好");
    }

    #[test]
    fn distinct_beam_fills_the_width_past_duplicates() {
        let mut sorted = Vec::new();
        for index in 0..BEAM + 3 {
            let text = index.to_string();
            let score = -(index as f64);
            sorted.push(state(&text, score));
            sorted.push(state(&text, score - 0.5));
        }
        let kept = distinct_beam(sorted.into_iter(), BEAM);
        assert_eq!(kept.len(), BEAM);
        assert_eq!(kept[BEAM - 1].text, (BEAM - 1).to_string());
    }

    #[test]
    fn tail_continues_the_committed_text() {
        assert_eq!(tail("今天天气", ""), "天天气");
        assert_eq!(tail("今天天气", "很"), "天气很");
        assert_eq!(tail("", "你好吗呀"), "好吗呀");
    }
}
