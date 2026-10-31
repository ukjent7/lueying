//! 跨引擎共享语料基准:同一份冻结语料、同一套归一化,输出可并排比较的逐条结果。
//!
//! 语料 `dicts/eval-shared-v1.tsv` 只由外部来源组成,列定义与适配器见
//! `tools/bench_shared.py`。本模块只负责跑掠影这一侧,并写出:
//! - `target/bench-shared.tsv`:统一逐条结果,可直接交给 `bench_shared.py compare`
//! - `target/bench-shared.json`:同一批数据的结构化版本,便于归档与看板
//!
//! 门限按类别分别卡,避免中立难例把日常集的变化稀释掉。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_engine::{Engine, Fuzzy, Key};
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank::Models;

const CORPUS: &str = include_str!("../../../dicts/eval-shared-v1.tsv");
const CORPUS_NAME: &str = "shared-v1";
const SCHEMA: &str = "lueying-bench-v1";

/// 每类的最低线:只防整条链路系统性退步,数值与全量基准的外部下限一致。
const GATES: &[(&str, f64, f64)] = &[
    ("daily", 85.0, 90.0),
    ("frost", 65.0, 80.0),
    ("hard", 50.0, 75.0),
];

struct Case<'a> {
    category: &'a str,
    input: &'a str,
    expected: &'a str,
}

struct Item<'a> {
    category: &'a str,
    input: &'a str,
    expected: &'a str,
    rank: i64,
    top1: String,
    ms: f64,
}

#[derive(Default)]
struct Metric {
    n: usize,
    top1: usize,
    top3: usize,
    top5: usize,
    rr: f64,
    times: Vec<f64>,
}

impl Metric {
    fn add(&mut self, rank: Option<usize>, ms: f64) {
        self.n += 1;
        if let Some(rank) = rank {
            if rank == 0 {
                self.top1 += 1;
            }
            if rank < 3 {
                self.top3 += 1;
            }
            if rank < 5 {
                self.top5 += 1;
            }
            self.rr += 1.0 / (rank + 1) as f64;
        }
        self.times.push(ms);
    }

    fn rate(&self, hits: usize) -> f64 {
        hits as f64 * 100.0 / self.n.max(1) as f64
    }

    fn mrr(&self) -> f64 {
        self.rr / self.n.max(1) as f64
    }

    fn ms_avg(&self) -> f64 {
        self.times.iter().sum::<f64>() / self.times.len().max(1) as f64
    }

    fn ms_p99(&self) -> f64 {
        if self.times.is_empty() {
            return 0.0;
        }
        let mut sorted = self.times.clone();
        sorted.sort_by(f64::total_cmp);
        let index = ((sorted.len() - 1) as f64 * 0.99).round() as usize;
        sorted[index.min(sorted.len() - 1)]
    }
}

fn cases() -> (Vec<Case<'static>>, usize) {
    let mut rows = Vec::new();
    let mut malformed = 0;
    for line in CORPUS.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(category), Some(_source), Some(input), Some(expected)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            malformed += 1;
            continue;
        };
        rows.push(Case {
            category,
            input,
            expected,
        });
    }
    (rows, malformed)
}

fn escape(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

fn metric_json(metric: &Metric) -> serde_json::Value {
    serde_json::json!({
        "n": metric.n,
        "top1": metric.rate(metric.top1),
        "top3": metric.rate(metric.top3),
        "top5": metric.rate(metric.top5),
        "mrr": metric.mrr(),
        "ms_avg": metric.ms_avg(),
        "ms_p99": metric.ms_p99(),
    })
}

fn row(text: &mut String, label: &str, metric: &Metric) {
    let _ = writeln!(
        text,
        "| {label} | {} | {:.1}% | {:.1}% | {:.1}% | {:.3} | {:.2} | {:.2} |",
        metric.n,
        metric.rate(metric.top1),
        metric.rate(metric.top3),
        metric.rate(metric.top5),
        metric.mrr(),
        metric.ms_avg(),
        metric.ms_p99(),
    );
}

/// 与 `bench::eval_texts` 同口径的候选排名:快捷项与固定项不占名额。
fn rank_of(engine: &Engine, expected: &str) -> Option<usize> {
    bench::eval_texts(engine)
        .into_iter()
        .position(|text| bench::matches_normalized(text, expected))
}

fn pct(values: &mut [f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[((p / 100.0) * (values.len() - 1) as f64).round() as usize]
}

/// 在候选顺序上模拟一次「提升到首位」,返回期望文本的名次。
/// `kept` 与 `order` 同长,只有 `eval_texts` 保留的候选占名额。
fn rank_after(
    order: &[String],
    kept: &[bool],
    promote: Option<usize>,
    expected: &str,
) -> Option<usize> {
    let mut indices: Vec<usize> = (0..order.len())
        .filter(|&index| kept.get(index).copied().unwrap_or(false))
        .collect();
    if let Some(at) = promote.filter(|&at| kept.get(at).copied().unwrap_or(false)) {
        indices.retain(|&index| index != at);
        indices.insert(0, at);
    }
    indices
        .into_iter()
        .position(|index| bench::matches_normalized(&order[index], expected))
}

/// 一条候选链路的还原:4M 逐键同步、25M 停顿,与产品里的应用顺序一致。
/// 同时算出「不经过 4M、25M 直接跑在引擎顺序上」的名次,用来判断同步重排
/// 是否值得:它换来的即时顺序如果让停顿结果也变差,就该让位。
/// 三个模型耗时分开返回,由调用方汇进各自的分位向量。
fn apply_rerank(
    engine: &mut Engine,
    four: &mut Rerank,
    five: &mut Rerank,
    expected: &str,
) -> (
    Option<usize>,
    Option<usize>,
    Option<usize>,
    Option<[f64; 3]>,
) {
    // 快捷与置顶候选的位置由插入顺序决定,产品链路直接跳过重排;
    // 没有模型调用,也就没有耗时样本。
    if engine
        .candidates_detailed()
        .iter()
        .any(|cand| cand.quick || cand.pinned)
    {
        let rank = rank_of(engine, expected);
        return (rank, rank, rank, None);
    }
    let snap = crate::rerank::snapshot(engine);
    let kept: Vec<bool> = engine
        .candidates_detailed()
        .iter()
        .map(|cand| !cand.no_learn && !cand.quick)
        .collect();
    let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
    let started = Instant::now();
    let promote = four.decide(&snap.context, &texts, &snap.facts);
    let four_ms = started.elapsed().as_secs_f64() * 1000.0;
    if let Some(index) = promote {
        engine.promote(index);
    }
    let rank4 = rank_of(engine, expected);

    let snap_after = crate::rerank::snapshot(engine);
    let texts: Vec<&str> = snap_after.texts.iter().map(String::as_str).collect();
    let started = Instant::now();
    let promote = five.decide_settled(
        &snap_after.context,
        &texts,
        &snap_after.facts,
        &snap_after.fuzzy,
    );
    let five_ms = started.elapsed().as_secs_f64() * 1000.0;
    if let Some(index) = promote {
        engine.promote(index);
    }
    let rank5 = rank_of(engine, expected);

    // 对照组:同一份 25M,不经过 4M,候选仍是引擎顺序。
    let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
    let started = Instant::now();
    let promote = five.decide_settled(&snap.context, &texts, &snap.facts, &snap.fuzzy);
    let direct_ms = started.elapsed().as_secs_f64() * 1000.0;
    let direct = rank_after(&snap.texts, &kept, promote, expected);
    (rank4, rank5, direct, Some([four_ms, five_ms, direct_ms]))
}

/// 单条用例的评测结果;分片线程各自产出,主线程按原序合并进指标。
/// 重排部分是 `((4M,25M,仅25M) 名次, 各模型耗时)`;捷径路径没有耗时样本。
struct Outcome {
    rank: Option<usize>,
    ms: f64,
    top1: String,
    rerank: Option<(
        (Option<usize>, Option<usize>, Option<usize>),
        Option<[f64; 3]>,
    )>,
}

/// 一片用例的评测;每片自带引擎与模型句柄,片间无共享可变状态,
/// 结果与单线程逐条跑完全一致。
fn run_chunk(
    chunk: &[Case],
    dict: &Arc<Dictionary>,
    gram: &Option<Arc<Gram>>,
    models: Option<&Models>,
) -> Vec<Outcome> {
    let mut engine = bench::make_engine(dict, gram, Fuzzy::auto());
    let mut four = models.map(|m| Rerank::new(&m.four));
    let mut five = models.map(|m| Rerank::new(&m.five));
    let mut out = Vec::with_capacity(chunk.len());
    for case in chunk {
        engine.process(Key::Esc);
        engine.clear_context();
        let started = Instant::now();
        for c in case.input.chars() {
            engine.process(Key::Letter(c));
        }
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let candidates = bench::eval_texts(&engine);
        let expected = bench::normalize(case.expected);
        let rank = candidates
            .iter()
            .position(|text| bench::matches_normalized(text, &expected));
        let top1 = candidates
            .first()
            .map_or(String::new(), |text| (*text).to_string());
        let rerank = four.as_mut().zip(five.as_mut()).map(|(four, five)| {
            let (ranks, times) = apply_rerank(&mut engine, four, five, &expected);
            (ranks, times)
        });
        out.push(Outcome {
            rank,
            ms,
            top1,
            rerank,
        });
    }
    out
}

pub fn run(
    dict: Arc<Dictionary>,
    gram: Option<Arc<Gram>>,
    models: Option<&Models>,
    report: &mut String,
) -> usize {
    let (cases, malformed) = cases();
    let mut failures = (malformed > 0) as usize + cases.is_empty() as usize;
    let gram_on = gram.is_some();
    let mut by_category: BTreeMap<&str, Metric> = BTreeMap::new();
    let mut overall = Metric::default();
    let mut core = Metric::default();
    let mut items = Vec::with_capacity(cases.len());
    // 产品链路:4M 逐键同步,25M 停顿。只在提供了模型目录时测量。
    let mut four_by_category: BTreeMap<&str, Metric> = BTreeMap::new();
    let mut five_by_category: BTreeMap<&str, Metric> = BTreeMap::new();
    let mut direct_by_category: BTreeMap<&str, Metric> = BTreeMap::new();
    let mut four_overall = Metric::default();
    let mut five_overall = Metric::default();
    let mut direct_overall = Metric::default();
    let mut four_core = Metric::default();
    let mut five_core = Metric::default();
    let mut direct_core = Metric::default();
    let mut four_ms = Vec::new();
    let mut five_ms = Vec::new();
    let mut direct_ms = Vec::new();

    // 用例互不依赖,按核数分片并行;每条约 0.8s,单线程 1444 条要近 20 分钟,
    // 是 CI 的最大单项。模型权重在 Arc 后共享,每线程只多一份轻量句柄。
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .max(1);
    let chunk_size = cases.len().div_ceil(threads).max(1);
    let dict_ref = &dict;
    let gram_ref = &gram;
    let outcomes: Vec<Vec<Outcome>> = std::thread::scope(|scope| {
        let handles: Vec<_> = cases
            .chunks(chunk_size)
            .map(|chunk| scope.spawn(move || run_chunk(chunk, dict_ref, gram_ref, models)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("共享语料基准分片线程"))
            .collect()
    });

    for (case, outcome) in cases.iter().zip(outcomes.into_iter().flatten()) {
        let Outcome {
            rank,
            ms,
            top1,
            rerank,
        } = outcome;
        by_category.entry(case.category).or_default().add(rank, ms);
        overall.add(rank, ms);
        if case.category != "hard" {
            core.add(rank, ms);
        }
        if let Some(((r4, r5, direct), times)) = rerank {
            four_by_category
                .entry(case.category)
                .or_default()
                .add(r4, ms);
            five_by_category
                .entry(case.category)
                .or_default()
                .add(r5, ms);
            direct_by_category
                .entry(case.category)
                .or_default()
                .add(direct, ms);
            four_overall.add(r4, ms);
            five_overall.add(r5, ms);
            direct_overall.add(direct, ms);
            if case.category != "hard" {
                four_core.add(r4, ms);
                five_core.add(r5, ms);
                direct_core.add(direct, ms);
            }
            if let Some([t4, t5, td]) = times {
                four_ms.push(t4);
                five_ms.push(t5);
                direct_ms.push(td);
            }
        }
        items.push(Item {
            category: case.category,
            input: case.input,
            expected: case.expected,
            rank: rank.map_or(-1, |index| index as i64),
            top1,
            ms,
        });
    }

    let mut text = String::from("## 跨引擎共享语料基准(shared-v1)\n\n");
    let _ = writeln!(
        text,
        "固定 {} 条外部语料,同一套归一化;逐条结果写入 `target/bench-shared.tsv` / `.json`。\n",
        cases.len()
    );
    text.push_str("| 类别 | 条数 | top-1 | top-3 | top-5 | MRR | 平均 ms | p99 ms |\n");
    text.push_str("|---|---:|---:|---:|---:|---:|---:|---:|\n");
    for (category, metric) in &by_category {
        row(&mut text, category, metric);
    }
    row(&mut text, "核心(daily+frost)", &core);
    row(&mut text, "全量", &overall);
    if models.is_some() {
        let line =
            |text: &mut String, label: &str, four: &Metric, five: &Metric, direct: &Metric| {
                let _ = writeln!(
                    text,
                    "| {label} | {} | {:.1}% | {:.1}% | {:.1}% | {:.1}% |",
                    five.n,
                    four.rate(four.top1),
                    five.rate(five.top1),
                    five.rate(five.top5),
                    direct.rate(direct.top1),
                );
            };
        text.push_str(
            "\n### 神经重排(产品链路)\n\n\
             4M 在每键返回前同步重排,+25M 是随后的停顿重排;两者只换顺序,不增删候选。\n\
             最后一列是同一份 25M 不经过 4M、直接在引擎顺序上决策的对照。\n\n\
             | 类别 | 条数 | 4M top-1 | +25M top-1 | +25M top-5 | 仅 25M top-1 |\n\
             |---|---:|---:|---:|---:|---:|\n",
        );
        for category in four_by_category.keys() {
            let (Some(four), Some(five), Some(direct)) = (
                four_by_category.get(*category),
                five_by_category.get(*category),
                direct_by_category.get(*category),
            ) else {
                continue;
            };
            line(&mut text, category, four, five, direct);
        }
        line(
            &mut text,
            "核心(daily+frost)",
            &four_core,
            &five_core,
            &direct_core,
        );
        line(
            &mut text,
            "全量",
            &four_overall,
            &five_overall,
            &direct_overall,
        );
        let four_p95 = pct(&mut four_ms.clone(), 95.0);
        let five_p95 = pct(&mut five_ms.clone(), 95.0);
        let _ = writeln!(
            text,
            "\n4M 决策 p95 {four_p95:.2} ms,25M 决策 p95 {five_p95:.2} ms。"
        );
    }

    text.push_str("\n### 判定\n\n| 检查 | 结果 |\n|---|---|\n");
    for (category, min_top1, min_top5) in GATES {
        let (ok, note) = match by_category.get(*category) {
            Some(metric) => {
                let ok =
                    metric.rate(metric.top1) >= *min_top1 && metric.rate(metric.top5) >= *min_top5;
                (
                    ok,
                    format!("top-1 ≥ {min_top1:.0}%, top-5 ≥ {min_top5:.0}%"),
                )
            }
            None => (false, "语料缺这一类".to_string()),
        };
        if !ok && gram_on {
            failures += 1;
        }
        let _ = writeln!(
            text,
            "| {category}:{note}{} | {} |",
            if gram_on {
                ""
            } else {
                "(无语法模型,仅诊断)"
            },
            if ok { "PASS" } else { "FAIL" }
        );
    }
    let _ = writeln!(
        text,
        "| 语料格式 | {} |",
        if malformed == 0 {
            "PASS".to_string()
        } else {
            format!("FAIL: {malformed} 行不是 4 列")
        }
    );
    if models.is_some() {
        let base = overall.rate(overall.top1);
        let settled = five_overall.rate(five_overall.top1);
        let ok = settled + 2.0 >= base;
        if !ok {
            failures += 1;
        }
        let _ = writeln!(
            text,
            "| 25M 不拖低共享语料 top-1 | {} |",
            if ok {
                "PASS".to_string()
            } else {
                format!("FAIL: {base:.1}% → {settled:.1}%")
            }
        );
    }

    let mut categories_json = serde_json::Map::new();
    for (category, metric) in &by_category {
        categories_json.insert((*category).to_string(), metric_json(metric));
    }
    let category_map = |by: &BTreeMap<&str, Metric>| {
        let mut map = serde_json::Map::new();
        for (category, metric) in by {
            map.insert((*category).to_string(), metric_json(metric));
        }
        map
    };
    let rerank_json = if models.is_some() {
        let mut four_times = four_ms.clone();
        let mut five_times = five_ms.clone();
        let mut direct_times = direct_ms.clone();
        serde_json::json!({
            "4m": {
                "overall": metric_json(&four_overall),
                "core": metric_json(&four_core),
                "categories": category_map(&four_by_category),
                "ms_p95": pct(&mut four_times, 95.0),
            },
            "25m": {
                "overall": metric_json(&five_overall),
                "core": metric_json(&five_core),
                "categories": category_map(&five_by_category),
                "ms_p95": pct(&mut five_times, 95.0),
            },
            "25m_direct": {
                "overall": metric_json(&direct_overall),
                "core": metric_json(&direct_core),
                "categories": category_map(&direct_by_category),
                "ms_p95": pct(&mut direct_times, 95.0),
            },
        })
    } else {
        serde_json::Value::Null
    };
    let items_json: Vec<_> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "category": item.category,
                "input": item.input,
                "expected": item.expected,
                "rank": item.rank,
                "top1": item.top1.as_str(),
                "ms": item.ms,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "schema": SCHEMA,
        "engine": format!("lueying {}", env!("CARGO_PKG_VERSION")),
        "corpus": CORPUS_NAME,
        "kind": "items",
        "config": {
            "fuzzy": "auto",
            "gram": gram_on,
        },
        "categories": categories_json,
        "core": metric_json(&core),
        "overall": metric_json(&overall),
        "rerank": rerank_json,
        "items": items_json,
    });
    let _ = std::fs::create_dir_all("target");
    if let Err(error) = std::fs::write(
        "target/bench-shared.json",
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    ) {
        let _ = writeln!(text, "\n> 写 `target/bench-shared.json` 失败: {error}");
        failures += 1;
    }

    let mut tsv = String::new();
    let _ = writeln!(tsv, "# schema={SCHEMA}");
    let _ = writeln!(tsv, "# engine=lueying {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(tsv, "# corpus={CORPUS_NAME}");
    tsv.push_str("# kind=items\ncategory\tinput\texpected\trank\ttop1\tms\n");
    for item in &items {
        let _ = writeln!(
            tsv,
            "{}\t{}\t{}\t{}\t{}\t{:.3}",
            item.category,
            escape(item.input),
            escape(item.expected),
            item.rank,
            escape(&item.top1),
            item.ms,
        );
    }
    if let Err(error) = std::fs::write("target/bench-shared.tsv", tsv) {
        let _ = writeln!(text, "\n> 写 `target/bench-shared.tsv` 失败: {error}");
        failures += 1;
    }

    let _ = writeln!(
        text,
        "\n与其它引擎并排比较:\n\n```\npython3 tools/bench_shared.py compare target/bench-shared.tsv <其它结果.tsv>\n```"
    );
    report.push_str(&text);
    failures
}
