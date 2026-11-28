//! 转换准确率实验口:同一份语料同时报引擎原序、8M 同步、25M 停顿三档,
//! 并按「词/短语/整句 × 有无上文 × 全拼/简拼/混合」分组。
//!
//! 共享语料基准(`shared`)只产出逐条结果且不带上文列,汇总口径在 Python 里。
//! 个人化实验需要一个固定、可在改前改后重复调用的对照,所以这里把分组与
//! P@1/P@5 一起固定下来;它不改产品代码,学习默认关闭。
//!
//! 用法:
//! `lueying-bench conversion --dict <主词典> [--gram <搭配表>] [--rerank-dir <模型目录>]
//!  --corpus <语料.tsv> [--limit <条数>]`

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use lueying_core::{Fuzzy, Key};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank::{self, Models};
use crate::shared;

#[derive(Clone, Copy, Default)]
struct Score {
    total: usize,
    top1: usize,
    top5: usize,
}

impl Score {
    fn add(&mut self, rank: i64) {
        self.total += 1;
        self.top1 += usize::from(rank == 0);
        self.top5 += usize::from((0..5).contains(&rank));
    }

    fn top1_rate(self) -> f64 {
        100.0 * self.top1 as f64 / self.total.max(1) as f64
    }

    fn top5_rate(self) -> f64 {
        100.0 * self.top5 as f64 / self.total.max(1) as f64
    }
}

pub(crate) fn group_of(category: &str) -> (&'static str, &'static str) {
    let mut parts = category.split('-');
    let unit = match parts.next() {
        Some("word") => "词级",
        Some("chunk") => "短语/分段",
        Some("clause") => "整句",
        _ => "其它",
    };
    let reading = match parts.next() {
        Some("full") => "全拼",
        Some("initials") => "简拼",
        Some("mixed") => "混合",
        _ => "其它",
    };
    (unit, reading)
}

#[derive(Default)]
struct Bucket {
    engine: Score,
    eight: Score,
    settled: Score,
}

/// 一行逐条结果;上下文只记长度与是否为空,不把用户原文再抄一份。
struct Row {
    category: String,
    input: String,
    expected: String,
    context_chars: usize,
    context_empty: bool,
    rank_engine: i64,
    ms_typing: f64,
    rank_8m: i64,
    rank_settled: i64,
    ms_8m: f64,
    ms_25m: f64,
}

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn write_group(out: &mut String, title: &str, groups: &[(String, Bucket)], with_rerank: bool) {
    let _ = writeln!(
        out,
        "## {title}\n\n| 分组 | 样本 | 引擎 P@1 | 引擎 P@5 | 8M P@1 | 8M P@5 | 25M P@1 | 25M P@5 |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|");
    for (name, bucket) in groups {
        if with_rerank {
            let _ = writeln!(
                out,
                "| {name} | {} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.1}% |",
                bucket.engine.total,
                bucket.engine.top1_rate(),
                bucket.engine.top5_rate(),
                bucket.eight.top1_rate(),
                bucket.eight.top5_rate(),
                bucket.settled.top1_rate(),
                bucket.settled.top5_rate(),
            );
        } else {
            let _ = writeln!(
                out,
                "| {name} | {} | {:.1}% | {:.1}% | - | - | - | - |",
                bucket.engine.total,
                bucket.engine.top1_rate(),
                bucket.engine.top5_rate(),
            );
        }
    }
    out.push('\n');
}

pub fn run(args: &[String]) -> i32 {
    let (Some(dict_path), Some(corpus_path)) = (value(args, "--dict"), value(args, "--corpus"))
    else {
        eprintln!("conversion 需要 --dict 与 --corpus(可选 --gram、--rerank-dir、--limit)");
        return 2;
    };
    let limit = value(args, "--limit")
        .and_then(|text| text.parse().ok())
        .unwrap_or(usize::MAX);
    let (dict, _) = match Dictionary::from_path(Path::new(dict_path)) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("词典加载失败 {dict_path}: {error}");
            return 2;
        }
    };
    let gram =
        value(args, "--gram").and_then(|path| Gram::open(Path::new(path)).ok().map(Arc::new));
    let models: Option<Models> =
        value(args, "--rerank-dir").and_then(|dir| match rerank::load(Path::new(dir)) {
            Ok(models) => Some(models),
            Err(error) => {
                eprintln!("模型加载失败 {dir}: {error}");
                None
            }
        });
    let body = match std::fs::read_to_string(corpus_path) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("读语料失败 {corpus_path}: {error}");
            return 2;
        }
    };
    let leaked: &'static str = Box::leak(body.into_boxed_str());
    let (cases, malformed) = shared::cases(leaked);
    if cases.is_empty() {
        eprintln!("语料没有可用条目 {corpus_path}");
        return 2;
    }
    let dict = Arc::new(dict);
    let mut rows = Vec::with_capacity(cases.len().min(limit));
    let mut typing_ms = Vec::new();
    let mut eight_ms = Vec::new();
    let mut five_ms = Vec::new();
    let mut four = models.as_ref().map(|models| Rerank::new(&models.four));
    let mut five = models.as_ref().map(|models| Rerank::new(&models.five));
    for case in cases.iter().take(limit) {
        let mut engine = bench::make_engine(&dict, &gram, Fuzzy::auto());
        engine.set_learning(false);
        engine.process(Key::Esc);
        if case.context.is_empty() {
            engine.clear_context();
        } else {
            engine.set_context(case.context);
        }
        let typing_started = Instant::now();
        for c in case.input.chars() {
            let started = Instant::now();
            engine.process(Key::Letter(c));
            typing_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let ms_typing = typing_started.elapsed().as_secs_f64() * 1000.0;
        let expected = bench::normalize(case.expected);
        let rank_engine = shared::rank_of(&engine, &expected);
        let (rank_8m, rank_settled, ms_8m, ms_25m) = match (four.as_mut(), five.as_mut()) {
            (Some(four), Some(five)) => {
                let started = Instant::now();
                let gated = crate::rerank::snapshot(&engine);
                let texts: Vec<&str> = gated.texts.iter().map(String::as_str).collect();
                let promote = (!gated.context.is_empty())
                    .then(|| four.decide(&gated.context, &texts, &gated.facts))
                    .flatten();
                let ms_8m = started.elapsed().as_secs_f64() * 1000.0;
                if let Some(index) = promote {
                    engine.promote(index);
                }
                let rank_8m = shared::rank_of(&engine, &expected);
                let snap = crate::rerank::snapshot(&engine);
                let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
                let started = Instant::now();
                let promote = five.decide_settled_prefiltered(
                    &snap.context,
                    &texts,
                    &snap.facts,
                    None,
                    Some(&mut *four),
                );
                let ms_25m = started.elapsed().as_secs_f64() * 1000.0;
                if let Some(index) = promote {
                    engine.promote(index);
                }
                eight_ms.push(ms_8m);
                five_ms.push(ms_25m);
                (rank_8m, shared::rank_of(&engine, &expected), ms_8m, ms_25m)
            }
            _ => (rank_engine, rank_engine, 0.0, 0.0),
        };
        rows.push(Row {
            category: case.category.to_string(),
            input: case.input.to_string(),
            expected: case.expected.to_string(),
            context_chars: case.context.chars().count(),
            context_empty: case.context.is_empty(),
            rank_engine,
            ms_typing,
            rank_8m,
            rank_settled,
            ms_8m,
            ms_25m,
        });
    }

    // 三套分组:单元、阅读形态、单元 × 上文。
    let mut by_unit: Vec<(String, Bucket)> = Vec::new();
    let mut by_reading: Vec<(String, Bucket)> = Vec::new();
    let mut by_context: Vec<(String, Bucket)> = Vec::new();
    let mut all = Bucket::default();
    for row in &rows {
        let (unit, reading) = group_of(&row.category);
        let context = if row.context_empty {
            "无上文"
        } else {
            "有上文"
        };
        let keys = [
            (&mut by_unit, unit.to_string()),
            (&mut by_reading, reading.to_string()),
            (&mut by_context, format!("{unit}/{context}")),
        ];
        for (groups, name) in keys {
            let bucket = match groups.iter_mut().find(|(key, _)| *key == name) {
                Some((_, bucket)) => bucket,
                None => {
                    groups.push((name, Bucket::default()));
                    &mut groups.last_mut().expect("刚插入").1
                }
            };
            bucket.engine.add(row.rank_engine);
            bucket.eight.add(row.rank_8m);
            bucket.settled.add(row.rank_settled);
        }
        all.engine.add(row.rank_engine);
        all.eight.add(row.rank_8m);
        all.settled.add(row.rank_settled);
    }
    by_unit.sort_by(|a, b| a.0.cmp(&b.0));
    by_reading.sort_by(|a, b| a.0.cmp(&b.0));
    by_context.sort_by(|a, b| a.0.cmp(&b.0));

    let name = Path::new(corpus_path)
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let mut out = format!(
        "# 转换准确率实验({name})\n\n语料 {corpus_path},{} 条,坏行 {malformed};学习关闭,固定分词与重排门控。\n\n",
        rows.len()
    );
    let all_group = ("全部".to_string(), all);
    write_group(
        &mut out,
        "按单元",
        std::slice::from_ref(&all_group),
        models.is_some(),
    );
    write_group(&mut out, "按单元细类", &by_unit, models.is_some());
    write_group(&mut out, "按阅读形态", &by_reading, models.is_some());
    write_group(&mut out, "按单元 × 上文", &by_context, models.is_some());
    let _ = writeln!(
        out,
        "## 延迟(逐键,不含重排)\n\n| 指标 | 毫秒 |\n|---|---:|\n| 按键数 | {} |\n| p50 | {:.2} |\n| p95 | {:.2} |\n| p99 | {:.2} |",
        typing_ms.len(),
        shared::pct(&mut typing_ms, 50.0),
        shared::pct(&mut typing_ms, 95.0),
        shared::pct(&mut typing_ms, 99.0),
    );
    if models.is_some() {
        let _ = writeln!(
            out,
            "\n| 重排 | p50 | p95 | p99 |\n|---|---:|---:|---:|\n| 8M 同步 | {:.2} | {:.2} | {:.2} |\n| 25M 停顿 | {:.2} | {:.2} | {:.2} |",
            shared::pct(&mut eight_ms, 50.0),
            shared::pct(&mut eight_ms, 95.0),
            shared::pct(&mut eight_ms, 99.0),
            shared::pct(&mut five_ms, 50.0),
            shared::pct(&mut five_ms, 95.0),
            shared::pct(&mut five_ms, 99.0),
        );
    }
    let _ = std::fs::create_dir_all("target/bench");
    let raw_path = format!("target/bench/conversion-{name}.tsv");
    let mut raw = String::from(
        "category\tinput\texpected\tcontext_chars\tcontext_empty\trank_engine\trank_8m\trank_settled\tms_typing\tms_8m\tms_25m\n",
    );
    for row in &rows {
        let _ = writeln!(
            raw,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}",
            row.category,
            row.input.replace(['\t', '\n'], " "),
            row.expected.replace(['\t', '\n'], " "),
            row.context_chars,
            u8::from(row.context_empty),
            row.rank_engine,
            row.rank_8m,
            row.rank_settled,
            row.ms_typing,
            row.ms_8m,
            row.ms_25m,
        );
    }
    if let Err(error) = std::fs::write(&raw_path, raw) {
        eprintln!("写 {raw_path} 失败: {error}");
        return 1;
    }
    let md_path = format!("target/conversion-{name}.md");
    let _ = std::fs::write(&md_path, &out);
    print!("{out}\n逐条结果: {raw_path}\n报告: {md_path}\n");
    0
}
