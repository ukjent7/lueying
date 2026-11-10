//! 学习模拟:模拟时钟注入,走产品链路量化学习的效果。
//!
//! - 新词:词典外的两字及以上词,第 1、2、3 次选中后的首选率;曝光间隔 0/30/90 天;末次之后闲置 90 天的名次。
//! - 误选:基线首选为 X、期望为 W;上屏 X、立刻删掉重打并改选 W,次日 X 是否重新首选。
//! - 衰减:3 次使用(背靠背,或每隔 30 天)后闲置 90 天;20 次使用跨 6 个月后闲置 90 天。
//! - 副作用(`--full`):用户整句训练 3 遍后,验收集 `eval-dev-v4` 的产品链路首选变化。
//! - 收益:用户整句每遍先测首选、再按正确选择学习,比较第 1 遍与第 3 遍。
//!
//! 用户语料(`eval-user-*`)只输出汇总数字,不逐句输出。用法:
//! `lueying-bench learn-sim --dict <主词典> --gram <搭配表> --rerank-dir <模型目录> [--full] [--limit <条数>]`

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use lueying_core::{Engine, Fuzzy, Key, set_sim_now};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_dict::syllables::is_syllable;
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank::{self, Models};
use crate::shared::{self, Case};

const DEV: &str = include_str!("../../../dicts/eval-dev-v4.tsv");
const BASE_SECS: u64 = 1_800_000_000;
const DAY: f64 = 86_400.0;
const IDLE_DAYS: f64 = 90.0;
/// 快速版每项的条数上限;完整版不设上限。
const FAST_LIMIT: usize = 100;

struct Ctx {
    dict: Arc<Dictionary>,
    gram: Option<Arc<Gram>>,
    models: Models,
}

impl Ctx {
    fn engine(&self, learning: bool) -> Engine {
        let mut engine = bench::make_engine(&self.dict, &self.gram, Fuzzy::auto());
        engine.set_learning(learning);
        engine
    }
}

fn secs(day: f64) -> u64 {
    BASE_SECS + (day * DAY) as u64
}

fn pct(part: usize, total: usize) -> f64 {
    100.0 * part as f64 / total.max(1) as f64
}

/// 上文为空也用 `set_context("")`:不清除未完成的误选记录(焦点切换才清)。
fn type_key(engine: &mut Engine, context: &str, input: &str) {
    engine.process(Key::Esc);
    engine.set_context(context);
    for c in input.chars() {
        engine.process(Key::Letter(c));
    }
}

fn index_of(engine: &Engine, expected: &str) -> Option<usize> {
    let want = bench::normalize(expected);
    engine
        .candidates_detailed()
        .iter()
        .position(|cand| !cand.quick && bench::matches_normalized(&cand.text, &want))
}

/// 选中候选全表里的第 `index` 个:先翻页再按数字键,与产品按键路径一致(会触发学习)。
fn pick(engine: &mut Engine, index: usize) {
    let size = engine.page_size().max(1);
    for _ in 0..index / size {
        engine.process(Key::PageDown);
    }
    if let Ok(digit) = u8::try_from(index % size + 1) {
        engine.process(Key::Digit(digit));
    }
}

fn rank(engine: &Engine, word: &str) -> i64 {
    shared::rank_of(engine, &bench::normalize(word))
}

/// 与产品链路同口径的整句名次:8M 与 25M 两档重排之后的首选位置。
fn settled(engine: &mut Engine, case: &Case, four: &mut Rerank, five: &mut Rerank) -> i64 {
    type_key(engine, case.context, case.input);
    let expected = bench::normalize(case.expected);
    shared::apply_rerank(engine, four, five, &expected);
    shared::rank_of(engine, &expected)
}

/// 用一句的正确选择学习:按最长前缀逐段上屏。返回选词次数。
fn learn_sentence(engine: &mut Engine, sentence: &Case) -> usize {
    engine.process(Key::Esc);
    engine.set_context("");
    for c in sentence.input.chars() {
        engine.process(Key::Letter(c));
    }
    let mut rest = bench::normalize(sentence.expected);
    let mut picks = 0;
    for _ in 0..16 {
        let best = engine
            .candidates_detailed()
            .iter()
            .enumerate()
            .filter(|(_, cand)| !cand.quick && !cand.no_learn && !cand.text.is_empty())
            .filter(|(_, cand)| rest.starts_with(&bench::normalize(&cand.text)))
            .max_by_key(|(at, cand)| (cand.text.chars().count(), std::cmp::Reverse(*at)))
            .map(|(at, _)| at);
        let Some(at) = best else { break };
        let word = bench::normalize(&engine.candidates_detailed()[at].text);
        pick(engine, at);
        picks += 1;
        rest = rest[word.len()..].to_string();
        if rest.is_empty() || engine.composition().is_none() {
            break;
        }
    }
    picks
}

/// 输入串能否整段切成完整音节(贪心最长匹配)。切不开的段是简拼段。
fn has_abbreviation(input: &str) -> bool {
    let text: String = input
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let mut at = 0;
    while at < text.len() {
        let step = (1..=(text.len() - at).min(6))
            .rev()
            .find(|&len| is_syllable(&text[at..at + len]))
            .unwrap_or(0);
        if step == 0 {
            return true;
        }
        at += step;
    }
    false
}

/// 用户整句语料(四份合并);只在内存里用,不输出。
fn user_sentences(dir: &str) -> Vec<Case<'static>> {
    let mut out = Vec::new();
    for n in 1..=4 {
        let Ok(text) = std::fs::read_to_string(format!("{dir}/eval-user-v{n}.tsv")) else {
            continue;
        };
        let text: &'static str = Box::leak(text.into_boxed_str());
        out.extend(shared::cases(text).0);
    }
    out
}

/// 用户整句训练 `reps` 遍,第 r 遍在第 r 天;句与句之间隔两分钟。返回导出的档案与选词次数。
fn train_users(ctx: &Ctx, sentences: &[Case], reps: usize) -> (String, usize) {
    let mut engine = ctx.engine(true);
    let mut picks = 0;
    for rep in 0..reps {
        for (index, sentence) in sentences.iter().enumerate() {
            set_sim_now(secs(rep as f64) + index as u64 * 120);
            picks += learn_sentence(&mut engine, sentence);
        }
    }
    (engine.export_user(), picks)
}

/// 验收集的产品链路名次,按用例分片并行;每条用例一个新引擎,可选导入档案。
fn dev_ranks(ctx: &Ctx, cases: &[Case], saved: Option<&str>, day: f64) -> Vec<i64> {
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .max(1);
    let chunk = cases.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = cases
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    let mut four = Rerank::new(&ctx.models.four);
                    let mut five = Rerank::new(&ctx.models.five);
                    part.iter()
                        .map(|case| {
                            set_sim_now(secs(day));
                            let mut engine = ctx.engine(false);
                            if let Some(user) = saved {
                                engine.import_user(user);
                            }
                            settled(&mut engine, case, &mut four, &mut five)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("学习模拟分片线程"))
            .collect()
    })
}

/// 词典外的两字及以上词(chunk-full 类、键与词都不在主词典里)。
fn new_word_cases(ctx: &Ctx, limit: usize) -> Vec<Case<'static>> {
    let words: HashSet<(String, String)> = ctx
        .dict
        .iter()
        .map(|entry| (entry.key.replace('\'', ""), entry.word.to_string()))
        .collect();
    shared::cases(DEV)
        .0
        .into_iter()
        .filter(|case| case.category.starts_with("chunk-full-"))
        .filter(|case| case.expected.chars().count() >= 2)
        .filter(|case| !words.contains(&(case.input.to_string(), case.expected.to_string())))
        .take(limit)
        .collect()
}

#[derive(Default)]
struct NewWord {
    present: bool,
    natural_first: bool,
    first: [bool; 3],
    in_top3_after_third: bool,
    idle_rank: i64,
}

/// 第 1、2、3 次选中(间隔 `spacing` 天),每次之后同日量一次名次,末次之后闲置 90 天。
fn new_word(ctx: &Ctx, case: &Case, spacing: f64) -> NewWord {
    let (key, context, word) = (case.input, case.context, case.expected);
    set_sim_now(secs(0.0));
    let mut natural = ctx.engine(false);
    type_key(&mut natural, context, key);
    let mut engine = ctx.engine(true);
    type_key(&mut engine, context, key);
    let mut run = NewWord {
        present: index_of(&engine, word).is_some(),
        natural_first: rank(&natural, word) == 0,
        idle_rank: -1,
        ..NewWord::default()
    };
    if !run.present {
        return run;
    }
    for k in 1..=3usize {
        let day = (k - 1) as f64 * spacing;
        set_sim_now(secs(day));
        type_key(&mut engine, context, key);
        let Some(index) = index_of(&engine, word) else {
            break;
        };
        pick(&mut engine, index);
        set_sim_now(secs(day));
        type_key(&mut engine, context, key);
        let r = rank(&engine, word);
        run.first[k - 1] = r == 0;
        if k == 3 {
            run.in_top3_after_third = (0..3).contains(&r);
        }
    }
    set_sim_now(secs(2.0 * spacing + IDLE_DAYS));
    type_key(&mut engine, context, key);
    run.idle_rank = rank(&engine, word);
    run
}

fn part_new_word(ctx: &Ctx, limit: usize, out: &mut String) {
    let rows = new_word_cases(ctx, limit);
    let _ = writeln!(out, "### 新词(词典外两字及以上,n={})\n", rows.len());
    let _ = writeln!(
        out,
        "| 曝光间隔 | 在候选中 | 无学习首选 | 第1次后首选 | 第2次后首选 | 第3次后首选 | 第3次后前三 | 闲置90天后不在候选 | 闲置90天后首选 |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|");
    for spacing in [0.0, 30.0, 90.0] {
        let runs: Vec<NewWord> = rows
            .iter()
            .map(|case| new_word(ctx, case, spacing))
            .collect();
        let present: Vec<&NewWord> = runs.iter().filter(|run| run.present).collect();
        let n = present.len();
        let count = |f: fn(&NewWord) -> bool| present.iter().filter(|run| f(run)).count();
        let _ = writeln!(
            out,
            "| {spacing:.0} 天 | {n}/{} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% |",
            rows.len(),
            pct(count(|run| run.natural_first), n),
            pct(count(|run| run.first[0]), n),
            pct(count(|run| run.first[1]), n),
            pct(count(|run| run.first[2]), n),
            pct(count(|run| run.in_top3_after_third), n),
            pct(count(|run| run.idle_rank < 0), n),
            pct(count(|run| run.idle_rank == 0), n),
        );
    }
    out.push('\n');
}

/// 基线首选为 X、期望为 W 的样本:上屏 X,立刻删掉重打并改选 W,次日 X 是否重新首选。
fn part_mistake(ctx: &Ctx, limit: usize, out: &mut String) {
    let mut rows = Vec::new();
    set_sim_now(secs(0.0));
    for case in shared::cases(DEV)
        .0
        .into_iter()
        .filter(|c| c.category.starts_with("chunk-"))
    {
        let mut engine = ctx.engine(false);
        type_key(&mut engine, case.context, case.input);
        let Some(first) = bench::eval_texts(&engine)
            .first()
            .map(|text| (*text).to_string())
        else {
            continue;
        };
        if bench::matches_normalized(&first, &bench::normalize(case.expected))
            || index_of(&engine, case.expected).is_none()
        {
            continue;
        }
        rows.push((case, first));
        if rows.len() >= limit {
            break;
        }
    }
    let (mut today, mut next, mut total) = (0, 0, 0);
    for (case, x_text) in &rows {
        let mut engine = ctx.engine(true);
        let (key, context) = (case.input, case.context);
        set_sim_now(secs(0.0));
        type_key(&mut engine, context, key);
        let Some(x_index) = engine.candidates_detailed().iter().position(|cand| {
            !cand.quick && bench::matches_normalized(&cand.text, &bench::normalize(x_text))
        }) else {
            continue;
        };
        pick(&mut engine, x_index);
        type_key(&mut engine, context, key);
        let Some(w_index) = index_of(&engine, case.expected) else {
            continue;
        };
        pick(&mut engine, w_index);
        total += 1;
        // 以原文计首选(不归一化):她/他 之类的同音变体是另一个候选。
        let is_x = |engine: &Engine| {
            bench::eval_texts(engine)
                .first()
                .is_some_and(|text| *text == x_text)
        };
        set_sim_now(secs(0.0));
        type_key(&mut engine, context, key);
        today += usize::from(is_x(&engine));
        set_sim_now(secs(1.0));
        type_key(&mut engine, context, key);
        next += usize::from(is_x(&engine));
    }
    let _ = writeln!(out, "### 误选(基线首选为 X、期望 W,n={total})\n");
    let _ = writeln!(out, "| 当天 X 首选 | 次日 X 首选 |\n|---|---|");
    let _ = writeln!(
        out,
        "| {:.1}% | {:.1}% |\n",
        pct(today, total),
        pct(next, total)
    );
}

/// 衰减:(a) 3 次使用背靠背或每隔 30 天后闲置 90 天;(b) 20 次每 9 天一次后闲置 90 天。
fn part_decay(ctx: &Ctx, limit: usize, out: &mut String) {
    let rows: Vec<Case> = new_word_cases(ctx, limit);
    // (不在前三数, 仍在候选中数, 样本数)
    let mut back = (0, 0, 0);
    let mut spaced = (0, 0, 0);
    let mut long = (0, 0);
    for case in &rows {
        let (key, context, word) = (case.input, case.context, case.expected);
        set_sim_now(secs(0.0));
        let mut natural = ctx.engine(false);
        type_key(&mut natural, context, key);
        let natural_top3 = (0..3).contains(&rank(&natural, word));
        for (spacing, acc) in [(0.0, &mut back), (30.0, &mut spaced)] {
            let mut engine = ctx.engine(true);
            let mut present = true;
            for k in 0..3usize {
                set_sim_now(secs(k as f64 * spacing));
                type_key(&mut engine, context, key);
                match index_of(&engine, word) {
                    Some(index) => pick(&mut engine, index),
                    None => {
                        present = false;
                        break;
                    }
                }
            }
            if !present || natural_top3 {
                continue;
            }
            set_sim_now(secs(2.0 * spacing + IDLE_DAYS));
            type_key(&mut engine, context, key);
            let r = rank(&engine, word);
            acc.2 += 1;
            acc.0 += usize::from(!(0..3).contains(&r));
            acc.1 += usize::from(r >= 0);
        }
        let mut engine = ctx.engine(true);
        let mut present = true;
        for k in 0..20usize {
            set_sim_now(secs(k as f64 * 9.0));
            type_key(&mut engine, context, key);
            match index_of(&engine, word) {
                Some(index) => pick(&mut engine, index),
                None => {
                    present = false;
                    break;
                }
            }
        }
        if present {
            set_sim_now(secs(19.0 * 9.0 + IDLE_DAYS));
            type_key(&mut engine, context, key);
            long.1 += 1;
            long.0 += usize::from(rank(&engine, word) == 0);
        }
    }
    let _ = writeln!(out, "### 衰减(词典外新词,自然非前三的样本)\n");
    let _ = writeln!(
        out,
        "| 情形 | 样本 | 闲置 90 天后不在前三 | 闲置 90 天后仍在候选中 |\n|---|---|---|---|"
    );
    let _ = writeln!(
        out,
        "| 3 次背靠背后闲置 90 天 | {} | {:.1}% | {:.1}% |",
        back.2,
        pct(back.0, back.2),
        pct(back.1, back.2)
    );
    let _ = writeln!(
        out,
        "| 3 次每隔 30 天后闲置 90 天 | {} | {:.1}% | {:.1}% |",
        spaced.2,
        pct(spaced.0, spaced.2),
        pct(spaced.1, spaced.2)
    );
    let _ = writeln!(
        out,
        "| 20 次跨 6 个月后闲置 90 天:首选率 | {} | {:.1}% | - |\n",
        long.1,
        pct(long.0, long.1)
    );
}

/// 副作用:用户整句训练 3 遍后,验收集的产品链路首选相对无学习的变化。
fn part_side_effect(ctx: &Ctx, sentences: &[Case], out: &mut String) {
    let cases = shared::cases(DEV).0;
    let base = dev_ranks(ctx, &cases, None, 3.0);
    let (saved, picks) = train_users(ctx, sentences, 3);
    let learned = dev_ranks(ctx, &cases, Some(&saved), 3.0);
    let top1 = |ranks: &[i64]| ranks.iter().filter(|&&r| r == 0).count();
    let losses = base
        .iter()
        .zip(&learned)
        .filter(|(b, l)| **b == 0 && **l != 0)
        .count();
    let wins = base
        .iter()
        .zip(&learned)
        .filter(|(b, l)| **b != 0 && **l == 0)
        .count();
    let (before, after) = (
        pct(top1(&base), base.len()),
        pct(top1(&learned), learned.len()),
    );
    let _ = writeln!(
        out,
        "### 副作用(用户整句训练 3 遍后的验收集,n={})\n",
        cases.len()
    );
    let _ = writeln!(
        out,
        "| 方法 | 训练选词数 | 产品链路首选 | Δ(百分点) | 退步 | 进步 |\n|---|---|---|---|---|---|"
    );
    let _ = writeln!(out, "| 无学习 | 0 | {before:.2}% | 0 | 0 | 0 |");
    let _ = writeln!(
        out,
        "| 用户整句学习 | {picks} | {after:.2}% | {:+.2} | {losses} | {wins} |\n",
        after - before
    );
}

/// 收益:用户整句每遍先测产品链路首选,再按正确选择学习;比较第 1 遍与第 3 遍。
fn part_user_gain(ctx: &Ctx, sentences: &[Case], out: &mut String) {
    let abbrev: Vec<bool> = sentences
        .iter()
        .map(|s| has_abbreviation(s.input))
        .collect();
    let mut engine = ctx.engine(true);
    let mut four = Rerank::new(&ctx.models.four);
    let mut five = Rerank::new(&ctx.models.five);
    let _ = writeln!(
        out,
        "### 用户整句收益(n={},每遍先测后学)\n",
        sentences.len()
    );
    let _ = writeln!(
        out,
        "| 遍 | 产品链路首选 | 简拼句首选 | 简拼句数 |\n|---|---|---|---|"
    );
    let abbrev_total = abbrev.iter().filter(|a| **a).count();
    for pass in 0..3usize {
        let (mut ok, mut ok_abbrev) = (0, 0);
        for (index, sentence) in sentences.iter().enumerate() {
            set_sim_now(secs(pass as f64) + index as u64 * 120);
            let hit = usize::from(settled(&mut engine, sentence, &mut four, &mut five) == 0);
            ok += hit;
            ok_abbrev += hit * usize::from(abbrev[index]);
            learn_sentence(&mut engine, sentence);
        }
        let _ = writeln!(
            out,
            "| 第 {} 遍 | {:.1}% | {:.1}% | {abbrev_total} |",
            pass + 1,
            pct(ok, sentences.len()),
            pct(ok_abbrev, abbrev_total)
        );
    }
    out.push('\n');
}

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

/// 子命令入口;返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    let (Some(dict_path), Some(rerank_dir)) = (value(args, "--dict"), value(args, "--rerank-dir"))
    else {
        eprintln!("learn-sim 需要 --dict 与 --rerank-dir(可选 --gram)");
        return 2;
    };
    let full = args.iter().any(|a| a == "--full");
    let limit = value(args, "--limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(if full { usize::MAX } else { FAST_LIMIT });
    let (dict, _) = match Dictionary::from_path(Path::new(dict_path)) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("词典加载失败 {dict_path}: {error}");
            return 2;
        }
    };
    let gram =
        value(args, "--gram").and_then(|path| Gram::open(Path::new(path)).ok().map(Arc::new));
    let models = match rerank::load(Path::new(rerank_dir)) {
        Ok(models) => models,
        Err(error) => {
            eprintln!("模型加载失败: {error}");
            return 2;
        }
    };
    let ctx = Ctx {
        dict: Arc::new(dict),
        gram,
        models,
    };
    let data = value(args, "--dicts").unwrap_or("dicts");
    let sentences = user_sentences(data);
    if sentences.is_empty() {
        eprintln!("没有找到用户语料 {data}/eval-user-v*.tsv");
        return 2;
    }
    let mut out = String::from("## 学习模拟\n\n");
    part_new_word(&ctx, limit, &mut out);
    part_mistake(&ctx, limit, &mut out);
    part_decay(&ctx, limit, &mut out);
    if full {
        part_side_effect(&ctx, &sentences, &mut out);
    }
    part_user_gain(&ctx, &sentences, &mut out);
    print!("{out}");
    if std::fs::create_dir_all("target").is_ok() {
        let _ = std::fs::write("target/learn-sim.md", &out);
    }
    0
}
