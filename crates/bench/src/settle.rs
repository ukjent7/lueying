//! 停顿位小模型组句:不只在现有候选里换顺序,而是用小模型给的整句概率重新组合分段。
//!
//! 现状(对照臂):8M 在每次按键同步重排一次,停顿再让 25M 在**同一批候选**里换顺序。
//! 候选本身是解码器按词频+搭配分束选出来的,小模型只看到结果。
//!
//! 实验臂:停顿位把「分段」也交给小模型——按引擎候选树展开前 `breadth` 个首槽选择
//! (每个分支重新敲一遍同一串输入,读到剩余输入的新候选),拼出至多 `breadth^depth`
//! 条完整句子文本,用 8M 的每字符平均对数概率打总分,取最高的一条;若它的首槽选择与
//! 当前首位不同,就把那一条提升上来。
//!
//! 指标:`base_*` 是现状定稿(8M + 25M 重排),`compose_*` 是小模型组句。
//! `改` = 组句给出的句子与现状定稿不同;`错改` = 现状本来对、组句改成了错的。
//!
//! 用法:
//! `lueying-bench settle --dict <主词典> --rerank-dir <模型目录> --corpus <语料> [--cases N]
//!  [--breadth 3] [--depth 2] [--margin 0.05]`

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use lueying_core::{Fuzzy, Key};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::latency::thread_cpu_ms;
use crate::rerank;
use crate::shared;

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn number<T: std::str::FromStr>(args: &[String], flag: &str, fallback: T) -> T {
    value(args, flag)
        .and_then(|text| text.parse().ok())
        .unwrap_or(fallback)
}

/// 一条完整句子的组句结果:拼出来的文本、首槽选择、以及它覆盖整串输入时的槽数与搜索代价。
struct Composed {
    text: String,
    first: String,
}

/// 一次分支:从 `start` 起把整串输入按 `picks` 逐槽提升,返回该状态下的候选文本。
fn branch(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    raw: &str,
    picks: &[String],
) -> Vec<(String, usize, bool)> {
    let mut engine = bench::make_engine(dict, gram, Fuzzy::auto());
    engine.set_learning(false);
    for c in raw.chars() {
        engine.process(Key::Letter(c));
    }
    for pick in picks {
        let index = engine
            .candidates_detailed()
            .iter()
            .position(|cand| &cand.text == pick);
        let Some(index) = index else {
            return Vec::new();
        };
        if !engine.promote(index) {
            return Vec::new();
        }
    }
    engine
        .candidates_detailed()
        .iter()
        .filter(|cand| !cand.quick && !cand.no_learn && !cand.text.is_empty())
        .map(|cand| (cand.text.clone(), cand.consume, cand.full))
        .collect()
}

/// 按候选树展开 `breadth^depth` 条完整句子。
fn compose(
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    raw: &str,
    breadth: usize,
    depth: usize,
) -> Vec<Composed> {
    let mut out: Vec<Composed> = Vec::new();
    let mut frontier: Vec<(Vec<String>, String)> = vec![(Vec::new(), String::new())];
    for _ in 0..depth {
        let mut next: Vec<(Vec<String>, String)> = Vec::new();
        for (picks, prefix) in frontier {
            for (text, _consume, full) in branch(dict, gram, raw, &picks).into_iter().take(breadth)
            {
                let mut path = picks.clone();
                path.push(text.clone());
                let joined = format!("{prefix}{text}");
                if full {
                    out.push(Composed {
                        text: joined,
                        first: path.first().cloned().unwrap_or_default(),
                    });
                } else {
                    next.push((path, joined));
                }
            }
        }
        frontier = next;
    }
    out.sort_by_key(|composed| std::cmp::Reverse(composed.text.chars().count()));
    out.dedup_by(|a, b| a.text == b.text);
    out
}

/// 停顿位组句:跑一版语料,对照现状定稿(8M + 25M 重排)。
pub fn run(args: &[String]) -> i32 {
    let (Some(dict_path), Some(rerank_dir)) = (value(args, "--dict"), value(args, "--rerank-dir"))
    else {
        eprintln!("settle 需要 --dict 与 --rerank-dir(可选 --gram、--corpus、--cases)");
        return 2;
    };
    let corpus_path = value(args, "--corpus").unwrap_or("dicts/eval-dev-v4.tsv");
    let limit = number(args, "--cases", usize::MAX);
    let breadth = number(args, "--breadth", 3usize).max(1);
    let depth = number(args, "--depth", 2usize).max(1);
    let margin = number(args, "--margin", 0.05f64);
    let Ok((dict, _)) = Dictionary::from_path(Path::new(dict_path)) else {
        eprintln!("词典加载失败 {dict_path}");
        return 2;
    };
    let gram =
        value(args, "--gram").and_then(|path| Gram::open(Path::new(path)).ok().map(Arc::new));
    let Ok(models) = rerank::load(Path::new(rerank_dir)) else {
        eprintln!("模型加载失败 {rerank_dir}");
        return 2;
    };
    let Ok(body) = std::fs::read_to_string(corpus_path) else {
        eprintln!("读语料失败 {corpus_path}");
        return 2;
    };
    let leaked: &'static str = Box::leak(body.into_boxed_str());
    let (cases, malformed) = shared::cases(leaked);
    let dict = Arc::new(dict);
    let mut four = Rerank::new(&models.four);
    let mut five = Rerank::new(&models.five);

    let mut rows = String::from(
        "input\texpected\tbase_top1\tbase_rank\tcompose_top1\tcompose_rank\tchanged\tms_settle\tms_compose\n",
    );
    // 计数:定稿对/错、组句对/错、改动次数、其中被改错的次数(常用词被错改)。
    let (mut base_ok, mut compose_ok, mut changed, mut broke, mut fixed) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    // 天花板数据:组句空间里有没有正确答案、现有候选列表里有没有、以及何时真的压过首位。
    let (mut novel_total, mut beat_leader, mut composed_has_expected, mut missed_correct) =
        (0usize, 0usize, 0usize, 0usize);
    let mut settle_ms = Vec::new();
    let mut compose_ms = Vec::new();
    let mut compose_cpu = Vec::new();
    let mut total = 0usize;
    for case in cases.iter().take(limit) {
        let expected = bench::normalize(case.expected);
        let fresh = || {
            let mut engine = bench::make_engine(&dict, &gram, Fuzzy::auto());
            engine.set_learning(false);
            engine.set_context(case.context);
            for c in case.input.chars() {
                engine.process(Key::Letter(c));
            }
            engine
        };
        // 对照臂:现状的 8M 同步 + 25M 停顿重排。
        let mut engine = fresh();
        let started = Instant::now();
        let snap = rerank::snapshot(&engine);
        if !snap.context.is_empty() && !snap.texts.is_empty() {
            let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
            if let Some(index) = four.decide(&snap.context, &texts, &snap.facts) {
                engine.promote(index);
            }
        }
        let snap = rerank::snapshot(&engine);
        if !snap.context.is_empty() && !snap.texts.is_empty() {
            let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
            if let Some(index) = five.decide_settled_prefiltered(&snap.context, &texts, &snap.facts, None, Some(&mut four)) {
                engine.promote(index);
            }
        }
        let settle = started.elapsed().as_secs_f64() * 1000.0;
        let base_top1 = engine
            .candidates_detailed()
            .first()
            .map(|cand| cand.text.clone())
            .unwrap_or_default();
        let base_rank = shared::rank_of(&engine, &expected);

        // 实验臂:同样的 8M 门控,再用小模型补出候选列表里没有的组句,只在本模型
        // 认为它比现有首位更好(留 margin)时才顶上来;之后照常走 25M 停顿重排,
        // 这样比的完全是"组句有没有补上候选树漏掉的句子"。
        let mut engine = fresh();
        let snap = rerank::snapshot(&engine);
        if !snap.context.is_empty() && !snap.texts.is_empty() {
            let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
            if let Some(index) = four.decide(&snap.context, &texts, &snap.facts) {
                engine.promote(index);
            }
        }
        let cpu0 = thread_cpu_ms().unwrap_or(0.0);
        let started = Instant::now();
        let current: Vec<String> = engine
            .candidates_detailed()
            .iter()
            .filter(|cand| !cand.quick && !cand.no_learn)
            .map(|cand| cand.text.clone())
            .collect();
        let mut best: Option<(f64, Composed)> = None;
        let mut novel = 0usize;
        for composed in compose(&dict, &gram, case.input, breadth, depth) {
            let score = four
                .scores(&snap.context, &[composed.text.as_str()])
                .first()
                .copied()
                .unwrap_or(f32::MIN);
            let score = f64::from(score);
            if !current.contains(&composed.text) {
                novel += 1;
                novel_total += 1;
                if bench::matches_normalized(&composed.text, &expected) {
                    missed_correct += 1;
                }
            }
            if bench::matches_normalized(&composed.text, &expected) {
                composed_has_expected += 1;
            }
            if best.as_ref().is_none_or(|(top, _)| score > *top) {
                best = Some((score, composed));
            }
        }
        // 现有首位在本模型眼里的分数;组句要压过它才算"补上更好的句子"。
        let leader = current.first().map(String::as_str).unwrap_or_default();
        let leader_score = if leader.is_empty() {
            f64::MIN
        } else {
            f64::from(
                four.scores(&snap.context, &[leader])
                    .first()
                    .copied()
                    .unwrap_or(f32::MIN),
            )
        };
        let mut applied = false;
        if let Some((score, composed)) = &best
            && *score > leader_score + margin
            && !current.contains(&composed.text)
        {
            beat_leader += 1;
            let index = engine
                .candidates_detailed()
                .iter()
                .position(|cand| cand.text == composed.first);
            if let Some(index) = index {
                applied = engine.promote(index);
            }
        }
        let compose_ms_now = started.elapsed().as_secs_f64() * 1000.0;
        let compose_cpu_now = thread_cpu_ms().unwrap_or(cpu0) - cpu0;
        let snap = rerank::snapshot(&engine);
        if !snap.context.is_empty() && !snap.texts.is_empty() {
            let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
            if let Some(index) = five.decide_settled_prefiltered(&snap.context, &texts, &snap.facts, None, Some(&mut four)) {
                engine.promote(index);
            }
        }
        let compose_top1 = engine
            .candidates_detailed()
            .first()
            .map(|cand| cand.text.clone())
            .unwrap_or_default();
        let compose_rank = shared::rank_of(&engine, &expected);
        let base_ok_now = base_rank == 0;
        let compose_ok_now = compose_rank == 0;
        let differs = applied && compose_top1 != base_top1;
        base_ok += usize::from(base_ok_now);
        compose_ok += usize::from(compose_ok_now);
        changed += usize::from(differs);
        broke += usize::from(differs && base_ok_now && !compose_ok_now);
        fixed += usize::from(differs && !base_ok_now && compose_ok_now);
        settle_ms.push(settle);
        compose_ms.push(compose_ms_now);
        compose_cpu.push(compose_cpu_now);
        total += 1;
        let _ = writeln!(
            rows,
            "{}\t{}\t{}\t{base_rank}\t{}\t{compose_rank}\t{}\t{settle:.2}\t{compose_ms_now:.2}",
            case.input.replace(['\t', '\n'], " "),
            case.expected.replace(['\t', '\n'], " "),
            base_top1.replace(['\t', '\n'], " "),
            compose_top1.replace(['\t', '\n'], " "),
            usize::from(differs),
        );
        let _ = (novel, margin);
    }
    let mut out = format!(
        "# 停顿位小模型组句({corpus_path})\n\n{total} 句(坏行 {malformed}),学习关闭;8M 同步 + 停顿位组句对照 8M + 25M 重排。\n\n| 指标 | 定稿(8M+25M 重排) | 小模型组句 |\n|---|---:|---:|\n"
    );
    let _ = writeln!(
        out,
        "| 首选正确 | {} ({:.2}%) | {} ({:.2}%) |",
        base_ok,
        100.0 * base_ok as f64 / total.max(1) as f64,
        compose_ok,
        100.0 * compose_ok as f64 / total.max(1) as f64
    );
    let _ = writeln!(
        out,
        "| 改动句数 | - | {changed}(改对 {fixed} / 改错 {broke}) |\n"
    );
    let _ = writeln!(
        out,
        "| 停顿耗时 | p50 {:.2} ms / p95 {:.2} ms | p50 {:.2} ms / p95 {:.2} ms |\n",
        shared::pct(&mut settle_ms.clone(), 50.0),
        shared::pct(&mut settle_ms.clone(), 95.0),
        shared::pct(&mut compose_ms.clone(), 50.0),
        shared::pct(&mut compose_ms.clone(), 95.0)
    );
    let _ = writeln!(
        out,
        "| 停顿线程 CPU | - | p50 {:.2} ms / p95 {:.2} ms |\n",
        shared::pct(&mut compose_cpu.clone(), 50.0),
        shared::pct(&mut compose_cpu.clone(), 95.0)
    );
    let _ = writeln!(
        out,
        "| 组句空间里的新句子 | - | {novel_total} 条({:.2} 条/句) |\n| 其中就是正确答案 | - | {missed_correct} 条(现有候选树漏掉的正确整句) |\n| 组句空间含正确答案的句数 | - | {composed_has_expected} |\n| 小模型判定该压过首位的次数 | - | {beat_leader} |\n",
        novel_total as f64 / total.max(1) as f64
    );
    print!("{out}");
    if std::fs::create_dir_all("target/bench").is_ok() {
        let _ = std::fs::write("target/bench/settle.tsv", rows);
    }
    0
}
