//! 候选准确率基准:固定外部语料与按键序列,量化每一层候选排序因素。
//!
//! 语料全部来自仓库外,本项目词典只生成候选、不参与造题:
//! - 词级:Microsoft Windows 经典示例的 2998 条全拼词表(MIT),经 msime 固化;
//! - 手写整句:项目内 80 条 + msime/WindInput 的 247 条(GPL-3.0);
//! - 白霜脚本:rime-frost 自带 93 条用例(GPL-3.0),单独成表;
//! - 难例:msime 中立收割的 1105 条(C4,ODC-BY),用于比较语言模型/重排器。
//!
//! 判定沿用 gaboolic/rime-schema-compare 的归一化:其它→其他、他/她/它→他、
//! 的/地/得→的,避免拼音本身不可分的写法被当成错。
//!
//! 同一份报告里比较模糊音、语法模型与长期学习;神经重排的 4M/25M 指标由同一次
//! E2E 的「神经重排」节给出。绝对准确率按语料来源分表,难例表的绝对值不可与
//! 手写集相减——它按构造全是引擎原序会错的句子。
//!
//! 参考项目的经验直接写进选材:不用项目词典出题(msime),n=60 的置信半宽约
//! ±13 点、模型对比要用千条级集合(qingjian),测量工具看 delta 而非单次汇总
//! (WindInput)。语料冻结在仓库里,改题就换文件名/版本,避免两次 CI 不可比。

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_dict::syllables::is_syllable;
use lueying_engine::{Config, Engine, Event, Fuzzy, Key};

const LONG_TERM_WORDS: usize = 60;
const LONG_TERM_ROUNDS: usize = 6;
/// 长期学习从主词典高频词里选可学会的歧义词;准确率套件仍用外部语料。
const LEARN_WORD_LIMIT: usize = 2000;

#[derive(Clone, Copy, Default)]
struct Score {
    total: usize,
    top1: usize,
    top5: usize,
}

impl Score {
    fn add(&mut self, rank: Option<usize>) {
        self.total += 1;
        if rank == Some(0) {
            self.top1 += 1;
        }
        if rank.is_some_and(|i| i < 5) {
            self.top5 += 1;
        }
    }

    fn top1_rate(self) -> f64 {
        self.top1 as f64 * 100.0 / self.total.max(1) as f64
    }

    fn top5_rate(self) -> f64 {
        self.top5 as f64 * 100.0 / self.total.max(1) as f64
    }
}

struct Miss {
    typed: String,
    want: String,
    got: String,
    /// 前几条候选,用于判断是分词、词频还是模糊扩展导致的首选偏差。
    alternatives: Vec<String>,
}

/// 与 rime-schema-compare 对齐的同义写法归一化。
fn normalize_char(c: char) -> char {
    match c {
        '她' | '它' => '他',
        '地' | '得' => '的',
        _ => c,
    }
}

pub(crate) fn normalize(text: &str) -> String {
    text.chars().map(normalize_char).collect()
}

/// 期望已归一化时逐字比较,避免给每条候选分配一个 String。
pub(crate) fn matches_normalized(candidate: &str, expected: &str) -> bool {
    candidate.chars().map(normalize_char).eq(expected.chars())
}

/// 两列 `输入<TAB>期望`,跳过注释与空行。
fn corpus(text: &'static str) -> Vec<(String, String)> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (typed, want) = line.split_once('\t')?;
            Some((typed.to_string(), want.to_string()))
        })
        .collect()
}

/// 长期学习的目标词池:主词典高频、1~4 字、全拼音节。只用于模拟学习,
/// 不参与准确率统计,因此这里用词典自身选词不会让准确率表自证。
fn learn_pool(dict: &Dictionary, limit: usize) -> Vec<(String, String)> {
    let mut entries: Vec<_> = dict
        .iter()
        .filter(|e| e.freq > 0)
        .filter(|e| {
            let chars = e.word.chars().count();
            (1..=4).contains(&chars)
                && e.word
                    .chars()
                    .all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        })
        .filter(|e| e.key.split('\'').all(is_syllable))
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.freq));
    entries
        .into_iter()
        .take(limit)
        .map(|e| (accent_key(e.key, 3), e.word.to_string()))
        .collect()
}

/// 口音模拟:1 = 翘舌/平舌,2 = n/l、f/h、r/l 与前后鼻音,3 = 两者叠加。
fn accent_key(key: &str, profile: u8) -> String {
    key.split('\'')
        .map(|syllable| accent_syllable(syllable, profile))
        .collect::<Vec<_>>()
        .join("")
}

/// 没有词典键的整句按最长合法音节切分后再改口音;切分歧义无法完全消除,
/// 但比把整串当一个音节可靠。词语优先用词典里带隔音符的键(`accent_words`)。
fn accent_text(raw: &str, profile: u8) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while pos < raw.len() {
        let rest = &raw[pos..];
        let len = (1..=6.min(rest.len()))
            .rev()
            .find(|&len| is_syllable(&rest[..len]))
            .unwrap_or(1);
        out.push_str(&accent_syllable(&rest[..len], profile));
        pos += len;
    }
    out
}

/// 从词典取「词 + 去隔音符键」到带隔音符键的映射,只处理评测语料里出现的词。
fn build_accent_words(dict: &Dictionary, words: &[(String, String)]) -> Vec<(String, String)> {
    let wanted: HashSet<&str> = words.iter().map(|(_, word)| word.as_str()).collect();
    let mut keys: HashMap<(String, String), String> = HashMap::new();
    for entry in dict.iter() {
        if !wanted.contains(entry.word) {
            continue;
        }
        let bare = entry.key.replace('\'', "");
        keys.entry((entry.word.to_string(), bare))
            .or_insert_with(|| entry.key.to_string());
    }
    words
        .iter()
        .map(|(typed, want)| {
            let accented = keys
                .get(&(want.clone(), typed.clone()))
                .map_or_else(|| accent_text(typed, 3), |key| accent_key(key, 3));
            (accented, want.clone())
        })
        .collect()
}

fn accent_syllable(syllable: &str, profile: u8) -> String {
    let mut out = syllable.to_string();
    if profile & 1 != 0 {
        for (from, to) in [("zh", "z"), ("ch", "c"), ("sh", "s")] {
            if let Some(next) = replace_prefix(&out, from, to) {
                out = next;
                break;
            }
        }
    }
    if profile & 2 != 0 {
        for (from, to) in [("n", "l"), ("f", "h"), ("r", "l")] {
            if let Some(next) = replace_prefix(&out, from, to) {
                out = next;
                break;
            }
        }
        for (from, to) in [("ang", "an"), ("eng", "en"), ("ing", "in")] {
            if let Some(next) = replace_suffix(&out, from, to) {
                out = next;
                break;
            }
        }
    }
    out
}

/// 口音改写只在结果是合法音节时生效。`din`、`zua` 这类拼音图里不存在的串
/// 会让引擎连音节都切不出来,拿它测评口音召回只会得到假阴性。
fn replace_prefix(syllable: &str, from: &str, to: &str) -> Option<String> {
    let rest = syllable.strip_prefix(from)?;
    let candidate = format!("{to}{rest}");
    is_syllable(&candidate).then_some(candidate)
}

fn replace_suffix(syllable: &str, from: &str, to: &str) -> Option<String> {
    let rest = syllable.strip_suffix(from)?;
    let candidate = format!("{rest}{to}");
    is_syllable(&candidate).then_some(candidate)
}

pub(crate) fn make_engine(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    fuzzy: Fuzzy,
) -> Engine {
    let mut engine = Engine::new(
        Arc::clone(dict),
        Config {
            fuzzy,
            ..Config::default()
        },
    );
    if let Some(gram) = gram {
        engine = engine.with_gram(Arc::clone(gram));
    }
    engine
}

/// 跑一批 `(输入, 期望)`;返回分数与前 5 条未命中。
fn run_suite(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    fuzzy: Fuzzy,
    corpus: &[(String, String)],
) -> (Score, Vec<Miss>) {
    let mut engine = make_engine(dict, gram, fuzzy);
    let mut score = Score::default();
    let mut misses = Vec::new();
    for (typed, want) in corpus {
        engine.process(Key::Esc);
        engine.clear_context();
        for c in typed.chars() {
            engine.process(Key::Letter(c));
        }
        let cands = eval_texts(&engine);
        let want_norm = normalize(want);
        let rank = cands
            .iter()
            .position(|c| matches_normalized(*c, &want_norm));
        score.add(rank);
        if rank != Some(0) && misses.len() < 5 {
            misses.push(Miss {
                typed: typed.clone(),
                want: want.clone(),
                got: cands.first().copied().unwrap_or_default().to_string(),
                alternatives: cands.iter().take(8).copied().map(str::to_string).collect(),
            });
        }
    }
    (score, misses)
}

/// 准确率只统计中文候选:表情、符号与英文直通项不占前五名额。
pub(crate) fn eval_texts(engine: &Engine) -> Vec<&str> {
    engine
        .candidates_detailed()
        .iter()
        .filter(|cand| !cand.no_learn && !cand.quick)
        .map(|cand| cand.text.as_str())
        .collect()
}

/// 翻到候选所在页并点选;用于模拟用户纠正。
fn pick(engine: &mut Engine, word: &str) -> bool {
    let Some(index) = engine.candidates().iter().position(|c| c.as_str() == word) else {
        return false;
    };
    let page_size = engine.page_size().max(1);
    while engine.page() < index / page_size {
        if !engine.page_by(1) {
            return false;
        }
    }
    engine
        .pick_in_page(index % page_size)
        .iter()
        .any(|event| matches!(event, Event::Commit(text) if text == word))
}

/// 长期学习:每轮逐条查询,用户从候选里选中期望词,学习结果累积。
/// 第 3 轮结束后导出并重新导入用户词库,模拟一次进程重启。
/// 返回每轮成绩与「学了一轮仍未到首选」的样本,便于定位学不会的词。
fn long_term(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    corpus: &[(String, String)],
) -> (Vec<Score>, Vec<Miss>) {
    let mut engine = make_engine(dict, gram, Fuzzy::auto());
    let mut rounds = Vec::with_capacity(LONG_TERM_ROUNDS);
    let mut stuck = Vec::new();
    for round in 0..LONG_TERM_ROUNDS {
        if round == 3 {
            let saved = engine.export_user();
            engine = make_engine(dict, gram, Fuzzy::auto());
            engine.import_user(&saved);
        }
        let mut score = Score::default();
        for (typed, want) in corpus {
            engine.process(Key::Esc);
            engine.clear_context();
            for c in typed.chars() {
                engine.process(Key::Letter(c));
            }
            let cands = eval_texts(&engine);
            let rank = cands.iter().position(|c| *c == want.as_str());
            score.add(rank);
            if round == 1 && rank != Some(0) {
                stuck.push(Miss {
                    typed: typed.clone(),
                    want: want.clone(),
                    got: cands.first().copied().unwrap_or_default().to_string(),
                    alternatives: cands.into_iter().take(8).map(str::to_string).collect(),
                });
            }
            if !pick(&mut engine, want) {
                engine.process(Key::Esc);
            }
        }
        rounds.push(score);
    }
    (rounds, stuck)
}

/// 从口音语料里挑出「当前不在首选、但能翻到」的词,学习才有可量化空间。
/// 只选候选里真有的期望,否则 oracle 也无从选起。
fn ambiguous_corpus(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    corpus: &[(String, String)],
) -> Vec<(String, String)> {
    let mut engine = make_engine(dict, gram, Fuzzy::auto());
    let mut out = Vec::new();
    for (typed, want) in corpus {
        engine.process(Key::Esc);
        for c in typed.chars() {
            engine.process(Key::Letter(c));
        }
        let rank = eval_texts(&engine).iter().position(|c| *c == want.as_str());
        if rank.is_some_and(|i| i > 0 && i < 40) {
            out.push((typed.clone(), want.clone()));
        }
        if out.len() >= LONG_TERM_WORDS {
            break;
        }
    }
    out
}

fn row(text: &mut String, label: &str, score: Score) {
    let _ = writeln!(
        text,
        "| {label} | {:.1}% ({}/{}) | {:.1}% ({}/{}) | {} |",
        score.top1_rate(),
        score.top1,
        score.total,
        score.top5_rate(),
        score.top5,
        score.total,
        score.total
    );
}

fn misses(text: &mut String, misses: &[Miss]) {
    misses_titled(text, "未命中示例", misses);
}

fn misses_titled(text: &mut String, title: &str, misses: &[Miss]) {
    if misses.is_empty() {
        return;
    }
    let _ = write!(
        text,
        "\n{title}(附前 8 条候选,便于判断是分词、词频还是模糊扩展导致):\n\n\
         | 输入 | 期望 | 首选 | 候选 |\n|---|---|---|---|\n"
    );
    for miss in misses {
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} |",
            miss.typed,
            miss.want,
            miss.got,
            miss.alternatives.join(" / ")
        );
    }
}

fn change(before: Score, after: Score) -> (f64, f64) {
    (
        after.top1_rate() - before.top1_rate(),
        after.top5_rate() - before.top5_rate(),
    )
}

/// 快速档只跑确定性的等距子集;全量档保留完整语料。
#[derive(Clone, Copy)]
pub enum Mode {
    Fast,
    Full,
}

// 快速档每次 push 只取能稳定暴露系统性退步的样本量;全量语料留给夜间与手动。
// 项目内 80 句是绝对准确率门限,不参与裁剪。
const FAST_WORDS: usize = 120;
const FAST_EXTRA: usize = 60;
const FAST_FROST: usize = 40;
const FAST_NEUTRAL: usize = 80;
const FAST_ACCENT_SENTENCES: usize = 60;

impl Mode {
    fn cap(self, rows: Vec<(String, String)>, fast: usize) -> Vec<(String, String)> {
        if matches!(self, Self::Full) || rows.len() <= fast {
            return rows;
        }
        let step = rows.len().div_ceil(fast);
        rows.into_iter().step_by(step).take(fast).collect()
    }
}

/// 一份语料的三档对比;`gram` 存在时多跑一次默认档。
fn run_corpus(
    dict: &Arc<Dictionary>,
    no_gram: &Option<Arc<Gram>>,
    gram: &Option<Arc<Gram>>,
    corpus: &[(String, String)],
) -> (Score, Score, Score, Vec<Miss>) {
    let (base, _) = run_suite(dict, no_gram, Fuzzy::off(), corpus);
    let (fuzzy, fuzzy_miss) = run_suite(dict, no_gram, Fuzzy::auto(), corpus);
    let (default, miss) = if gram.is_some() {
        run_suite(dict, gram, Fuzzy::auto(), corpus)
    } else {
        (fuzzy, fuzzy_miss)
    };
    (base, fuzzy, default, miss)
}

/// 口音档只比较模糊关/自动,不参与语法模型对比。
fn run_accent(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    corpus: &[(String, String)],
) -> (Score, Score, Vec<Miss>) {
    let (off, _) = run_suite(dict, gram, Fuzzy::off(), corpus);
    let (auto, miss) = run_suite(dict, gram, Fuzzy::auto(), corpus);
    (off, auto, miss)
}

pub fn run(
    dict: Arc<Dictionary>,
    gram: Option<Arc<Gram>>,
    report: &mut String,
    mode: Mode,
) -> usize {
    let words = mode.cap(
        corpus(include_str!("../../../dicts/eval-words.tsv")),
        FAST_WORDS,
    );
    let clean = corpus(include_str!("../../../dicts/eval.txt"));
    let extra = mode.cap(
        corpus(include_str!("../../../dicts/eval-clean-extra.tsv")),
        FAST_EXTRA,
    );
    let frost = mode.cap(
        corpus(include_str!("../../../dicts/eval-frost.tsv")),
        FAST_FROST,
    );
    let neutral = mode.cap(
        corpus(include_str!("../../../dicts/eval-neutral.tsv")),
        FAST_NEUTRAL,
    );
    let accent_words = build_accent_words(&dict, &words);
    // 同一输入在两份手写集里可能有不同金标准;保留项目内那份,避免同一键两个答案。
    let mut clean_sentences = clean.clone();
    for row in &extra {
        if !clean_sentences.iter().any(|(typed, _)| typed == &row.0) {
            clean_sentences.push(row.clone());
        }
    }
    let accent_sentences = mode.cap(
        clean_sentences
            .iter()
            .map(|(typed, want)| (accent_text(typed, 3), want.clone()))
            .collect(),
        FAST_ACCENT_SENTENCES,
    );

    let no_gram: Option<Arc<Gram>> = None;
    // 各语料互不依赖,并行跑;每个线程自带 Engine,报告顺序仍由 join 顺序固定。
    let (word, clean_r, extra_r, frost_r, neutral_r, accent_word, accent_sent) =
        std::thread::scope(|scope| {
            let word_h = scope.spawn(|| run_corpus(&dict, &no_gram, &gram, &words));
            let clean_h = scope.spawn(|| run_corpus(&dict, &no_gram, &gram, &clean));
            let extra_h = scope.spawn(|| run_corpus(&dict, &no_gram, &gram, &extra));
            let frost_h = scope.spawn(|| run_corpus(&dict, &no_gram, &gram, &frost));
            let neutral_h = scope.spawn(|| run_corpus(&dict, &no_gram, &gram, &neutral));
            let accent_word_h = scope.spawn(|| run_accent(&dict, &gram, &accent_words));
            let accent_sent_h = scope.spawn(|| run_accent(&dict, &gram, &accent_sentences));
            (
                word_h.join().unwrap(),
                clean_h.join().unwrap(),
                extra_h.join().unwrap(),
                frost_h.join().unwrap(),
                neutral_h.join().unwrap(),
                accent_word_h.join().unwrap(),
                accent_sent_h.join().unwrap(),
            )
        });
    let (word_base, word_fuzzy, word_default, word_miss) = word;
    let (clean_base, clean_fuzzy, clean_default, clean_miss) = clean_r;
    let (extra_base, extra_fuzzy, extra_default, extra_miss) = extra_r;
    let (frost_base, frost_fuzzy, frost_default, frost_miss) = frost_r;
    let (neutral_base, neutral_fuzzy, neutral_default, neutral_miss) = neutral_r;
    let (accent_word_off, accent_word_auto, accent_word_miss) = accent_word;
    let (accent_sent_off, accent_sent_auto, accent_sent_miss) = accent_sent;

    let learn_words = learn_pool(&dict, LEARN_WORD_LIMIT);
    let long_corpus = ambiguous_corpus(&dict, &gram, &learn_words);
    let (rounds, long_stuck) = long_term(&dict, &gram, &long_corpus);

    let mut text = String::from("# 候选准确率基准\n\n");
    if matches!(mode, Mode::Fast) {
        text.push_str("> 快速档:各语料取确定性等距子集;全量结果见夜间或手动触发。\n\n");
    }
    text.push_str(concat!(
        "固定外部语料与按键序列,只切换候选排序因素;绝对准确率按来源分表,",
        "跨 CI 对比同一张表即可量化改动。判定沿用 rime-schema-compare 的归一化:",
        "其它→其他、他/她/它→他、的/地/得→的。神经重排另见同次 E2E 的「神经重排」节。\n\n"
    ));
    text.push_str("## 词级(外部微软词表)\n\n| 配置 | top-1 | top-5 | 样本 |\n|---|---|---|---|\n");
    row(&mut text, "精确基线(模糊关/语法关)", word_base);
    row(&mut text, "模糊自动/语法关", word_fuzzy);
    row(
        &mut text,
        if gram.is_some() {
            "当前默认(模糊自动/语法开)"
        } else {
            "当前默认(模糊自动/无语法模型)"
        },
        word_default,
    );
    misses(&mut text, &word_miss);

    text.push_str("\n## 句级(项目内手写)\n\n| 配置 | top-1 | top-5 | 样本 |\n|---|---|---|---|\n");
    row(&mut text, "精确基线(模糊关/语法关)", clean_base);
    row(&mut text, "模糊自动/语法关", clean_fuzzy);
    row(
        &mut text,
        if gram.is_some() {
            "当前默认(模糊自动/语法开)"
        } else {
            "当前默认(模糊自动/无语法模型)"
        },
        clean_default,
    );
    misses(&mut text, &clean_miss);

    text.push_str("\n## 句级(外部手写)\n\n| 配置 | top-1 | top-5 | 样本 |\n|---|---|---|---|\n");
    row(&mut text, "精确基线(模糊关/语法关)", extra_base);
    row(&mut text, "模糊自动/语法关", extra_fuzzy);
    row(
        &mut text,
        if gram.is_some() {
            "当前默认(模糊自动/语法开)"
        } else {
            "当前默认(模糊自动/无语法模型)"
        },
        extra_default,
    );
    misses(&mut text, &extra_miss);

    text.push_str("\n## 句级(白霜脚本)\n\n| 配置 | top-1 | top-5 | 样本 |\n|---|---|---|---|\n");
    row(&mut text, "精确基线(模糊关/语法关)", frost_base);
    row(&mut text, "模糊自动/语法关", frost_fuzzy);
    row(
        &mut text,
        if gram.is_some() {
            "当前默认(模糊自动/语法开)"
        } else {
            "当前默认(模糊自动/无语法模型)"
        },
        frost_default,
    );
    misses(&mut text, &frost_miss);

    text.push_str("\n## 句级难例(中立收割,只用于比较模型)\n\n| 配置 | top-1 | top-5 | 样本 |\n|---|---|---|---|\n");
    row(&mut text, "引擎原序(模糊关/语法关)", neutral_base);
    row(&mut text, "模糊自动/语法关", neutral_fuzzy);
    row(
        &mut text,
        if gram.is_some() {
            "当前默认(模糊自动/语法开)"
        } else {
            "当前默认(模糊自动/无语法模型)"
        },
        neutral_default,
    );
    misses(&mut text, &neutral_miss);

    text.push_str(
        "\n## 口音召回(同一份语料,按口音改写输入)\n\n| 配置 | top-1 | top-5 | 样本 |\n|---|---|---|---|\n",
    );
    row(&mut text, "词级/模糊关", accent_word_off);
    row(&mut text, "词级/模糊自动", accent_word_auto);
    row(&mut text, "句级/模糊关", accent_sent_off);
    row(&mut text, "句级/模糊自动", accent_sent_auto);
    misses(&mut text, &accent_word_miss);
    misses(&mut text, &accent_sent_miss);

    if gram.is_some() {
        let (clean_t1, clean_t5) = change(clean_fuzzy, clean_default);
        let (extra_t1, extra_t5) = change(extra_fuzzy, extra_default);
        let (frost_t1, frost_t5) = change(frost_fuzzy, frost_default);
        let (neutral_t1, neutral_t5) = change(neutral_fuzzy, neutral_default);
        text.push_str(&format!(
            "\n## 语法模型贡献\n\n| 语料 | top-1 变化 | top-5 变化 |\n|---|---:|---:|\n| 项目内手写 | {clean_t1:+.1} pt | {clean_t5:+.1} pt |\n| 外部手写 | {extra_t1:+.1} pt | {extra_t5:+.1} pt |\n| 白霜脚本 | {frost_t1:+.1} pt | {frost_t5:+.1} pt |\n| 中立难例 | {neutral_t1:+.1} pt | {neutral_t5:+.1} pt |\n"
        ));
    }

    text.push_str(&format!(
        "\n## 长期学习({} 个歧义词 × {} 轮)\n\n| 轮次 | top-1 | top-5 | 样本 |\n|---|---|---|---|\n",
        long_corpus.len(),
        LONG_TERM_ROUNDS
    ));
    for (index, score) in rounds.iter().enumerate() {
        row(
            &mut text,
            &format!(
                "第 {} 轮{}",
                index + 1,
                if index == 3 { "(重启后)" } else { "" }
            ),
            *score,
        );
    }
    misses_titled(&mut text, "长期学习仍未命中(第 2 轮起)", &long_stuck);

    let stable = |base: Score, current: Score| current.top1_rate() + 1.0 >= base.top1_rate();
    let word_ok = stable(word_base, word_default);
    let clean_ok = stable(clean_base, clean_default);
    let extra_ok = stable(extra_base, extra_default);
    let frost_ok = stable(frost_base, frost_default);
    let neutral_ok = stable(neutral_base, neutral_default);
    let quality_gate = if gram.is_some() { 92.0 } else { 90.0 };
    let quality_ok = clean_default.top1_rate() >= quality_gate && clean_default.top5_rate() >= 95.0;
    let accent_word_ok = accent_word_auto.top1_rate() + 0.5 >= accent_word_off.top1_rate()
        && accent_word_auto.top5_rate() + 0.5 >= accent_word_off.top5_rate()
        && (accent_word_auto.top1_rate() > accent_word_off.top1_rate()
            || accent_word_auto.top5_rate() > accent_word_off.top5_rate());
    let accent_sent_ok = accent_sent_auto.top1_rate() + 0.5 >= accent_sent_off.top1_rate()
        && accent_sent_auto.top5_rate() + 0.5 >= accent_sent_off.top5_rate()
        && (accent_sent_auto.top1_rate() > accent_sent_off.top1_rate()
            || accent_sent_auto.top5_rate() > accent_sent_off.top5_rate());
    let gram_ok = gram.is_none()
        || (clean_default.top1_rate() + 1.0 >= clean_fuzzy.top1_rate()
            && clean_default.top5_rate() + 1.0 >= clean_fuzzy.top5_rate()
            && extra_default.top1_rate() + 1.0 >= extra_fuzzy.top1_rate()
            && extra_default.top5_rate() + 1.0 >= extra_fuzzy.top5_rate()
            && frost_default.top1_rate() + 1.0 >= frost_fuzzy.top1_rate()
            && frost_default.top5_rate() + 1.0 >= frost_fuzzy.top5_rate()
            && neutral_default.top1_rate() + 1.0 >= neutral_fuzzy.top1_rate()
            && neutral_default.top5_rate() + 1.0 >= neutral_fuzzy.top5_rate());
    // 全量档才卡模型增益:快速档每份语料只取几十条,单条波动就能改变结论。
    let gram_gain = (extra_default.top1_rate() - extra_fuzzy.top1_rate())
        + (frost_default.top1_rate() - frost_fuzzy.top1_rate())
        + (neutral_default.top1_rate() - neutral_fuzzy.top1_rate());
    let gram_gain_ok = gram.is_none() || !matches!(mode, Mode::Full) || gram_gain >= 1.0;
    // 外部门限只防“整条链路一起退步”:项目内 80 句过了,外部也可能整体掉下去。
    // 数值按当前实测留出余量,低于它说明词典/解码/模型至少有一环出了系统性问题。
    let external_floor_ok = gram.is_none()
        || !matches!(mode, Mode::Full)
        || (extra_default.top1_rate() >= 85.0
            && extra_default.top5_rate() >= 90.0
            && frost_default.top1_rate() >= 65.0
            && frost_default.top5_rate() >= 80.0
            && neutral_default.top1_rate() >= 50.0
            && neutral_default.top5_rate() >= 75.0);
    let first = rounds.first().copied().unwrap_or_default();
    let last = rounds.last().copied().unwrap_or_default();
    let learning_ok = long_corpus.len() >= 10 && last.top1_rate() >= first.top1_rate() + 5.0;
    let restart = rounds.get(3).copied().unwrap_or_default();
    let before_restart = rounds.get(2).copied().unwrap_or_default();
    let restart_ok = restart.top1_rate() + 2.0 >= before_restart.top1_rate();

    let checks = [
        ("项目内句级绝对准确率达标", quality_ok),
        ("语法模型在全量外部语料上有增益", gram_gain_ok),
        ("全量外部语料绝对下限", external_floor_ok),
        ("长期学习有提升", learning_ok),
        ("重启后学习保留", restart_ok),
    ];
    // 精确基线与模糊默认的差异是产品取舍;口音语料还没按音节切分,当前只作趋势。
    let diagnostics = [
        ("词级外部集不回归", word_ok),
        ("项目内句级不回归", clean_ok),
        ("外部手写句级不回归", extra_ok),
        ("白霜脚本集不回归", frost_ok),
        ("中立难例不回归", neutral_ok),
        ("口音词级有提升", accent_word_ok),
        ("口音句级有提升", accent_sent_ok),
        ("语法模型不拖低各语料", gram_ok),
    ];
    text.push_str("\n## 判定\n\n| 检查 | 结果 |\n|---|---|\n");
    let mut failures = 0;
    for (name, ok) in checks {
        if !ok {
            failures += 1;
        }
        let _ = writeln!(text, "| {name} | {} |", if ok { "PASS" } else { "FAIL" });
    }
    text.push_str("\n### 诊断(不计入退出码)\n\n| 检查 | 结果 |\n|---|---|\n");
    for (name, ok) in diagnostics {
        let _ = writeln!(text, "| {name} | {} |", if ok { "PASS" } else { "FAIL" });
    }
    let (word_t1, word_t5) = change(accent_word_off, accent_word_auto);
    let (sent_t1, sent_t5) = change(accent_sent_off, accent_sent_auto);
    let learned_t1 = last.top1_rate() - first.top1_rate();
    let _ = writeln!(
        text,
        concat!(
            "\n口音词级 top-1 {word_t1:+.1} pt、top-5 {word_t5:+.1} pt;",
            "口音句级 top-1 {sent_t1:+.1} pt、top-5 {sent_t5:+.1} pt;",
            "长期学习 top-1 {learned_t1:+.1} pt。"
        ),
        word_t1 = word_t1,
        word_t5 = word_t5,
        sent_t1 = sent_t1,
        sent_t5 = sent_t5,
        learned_t1 = learned_t1
    );

    if let Some(dir) = std::path::Path::new("target/bench-report.md").parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write("target/bench-report.md", &text);
    report.push_str("\n## 候选准确率基准(模糊/语法/长期学习)\n\n详见 `target/bench-report.md`。\n");
    failures
}
