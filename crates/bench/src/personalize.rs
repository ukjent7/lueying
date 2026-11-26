//! 个性化实验口:同一初始档案,比较 flat(冻结)与 primed(在线学习)。
//!
//! 与 `learn-sim` 的区别:
//! - `learn-sim` 报新词、误选、衰减、用户整句收益与对开发集的副作用;
//! - 这里固定「先训练档案,再在留出语料上按时间顺序逐条先测后学」的口径,
//!   同时报 Top-1/P@5、逐条配对翻转与成功条件下的选词/翻页成本。
//!
//! 产品链路与共享语料基准一致:8M 有上文同步、25M 停顿。每次测量都先量分,
//! 再让模拟用户按最长前缀逐段上屏;上屏过程就是学习过程。
//!
//! 用法:
//! `lueying-bench personalize --dict <主词典> --test <语料> [--train <语料>]
//!  [--gram <搭配表>] [--rerank-dir <模型目录>] [--repeats N] [--limit N]`

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use lueying_core::{Engine, Fuzzy, Key, set_sim_now};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::conversion::group_of;
use crate::rerank::{self, Models};
use crate::shared::{self, Case};

const DAY: u64 = 86_400;
const BASE: u64 = 1_800_000_000;

#[derive(Default, Clone, Copy)]
struct Score {
    total: usize,
    top1: usize,
    top5: usize,
    picks: i64,
    pages: i64,
    unreachable: usize,
}

impl Score {
    fn add(&mut self, rank: i64, cost: (i64, i64)) {
        self.total += 1;
        self.top1 += usize::from(rank == 0);
        self.top5 += usize::from((0..5).contains(&rank));
        if cost.0 >= 0 {
            self.picks += cost.0;
            self.pages += cost.1;
        } else {
            self.unreachable += 1;
        }
    }

    fn top1_rate(self) -> f64 {
        100.0 * self.top1 as f64 / self.total.max(1) as f64
    }

    fn top5_rate(self) -> f64 {
        100.0 * self.top5 as f64 / self.total.max(1) as f64
    }

    fn avg_cost(self) -> f64 {
        (self.picks + self.pages) as f64 / (self.total - self.unreachable) as f64
    }
}

#[derive(Default, Clone, Copy)]
struct Flips {
    improved: [usize; 2],
    regressed: [usize; 2],
}

impl Flips {
    fn add(&mut self, before: i64, after: i64) {
        for (i, k) in [1, 5].into_iter().enumerate() {
            let was = (0..k).contains(&before);
            let now = (0..k).contains(&after);
            self.improved[i] += usize::from(now && !was);
            self.regressed[i] += usize::from(was && !now);
        }
    }
}

struct EngineSet {
    engine: Engine,
    four: Rerank,
    five: Rerank,
}

impl EngineSet {
    fn new(ctx: &Ctx, profile: Option<&str>, learning: bool) -> Self {
        let mut engine = ctx.engine(learning);
        if let Some(profile) = profile {
            engine.import_user(profile);
        }
        Self {
            engine,
            four: Rerank::new(&ctx.models.four),
            five: Rerank::new(&ctx.models.five),
        }
    }

    /// 设置上文并逐键输入;返回 8M/25M 重排后的期望名次。
    fn measure(&mut self, case: &Case) -> i64 {
        self.engine.process(Key::Esc);
        if case.context.is_empty() {
            self.engine.clear_context();
        } else {
            self.engine.set_context(case.context);
        }
        for c in case.input.chars() {
            self.engine.process(Key::Letter(c));
        }
        let expected = bench::normalize(case.expected);
        shared::apply_rerank(&mut self.engine, &mut self.four, &mut self.five, &expected);
        shared::rank_of(&self.engine, &expected)
    }

    fn cost(&mut self, case: &Case) -> (i64, i64) {
        shared::input_cost(&mut self.engine, &bench::normalize(case.expected))
    }
}

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

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn load_cases(path: &str) -> Vec<Case<'static>> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("读语料失败 {path}: {error}");
            return Vec::new();
        }
    };
    let body: &'static str = Box::leak(body.into_boxed_str());
    shared::cases(body).0
}

fn seed(ctx: &Ctx, profile: &[Case]) -> String {
    let mut set = EngineSet::new(ctx, None, true);
    for (index, case) in profile.iter().enumerate() {
        set_sim_now(BASE + index as u64 * DAY);
        set.measure(case);
        set.cost(case);
    }
    set.engine.export_user()
}

fn write_score(out: &mut String, label: &str, score: Score) {
    let _ = writeln!(
        out,
        "| {label} | {} | {:.1}% | {:.1}% | {} | {} |",
        score.total,
        score.top1_rate(),
        score.top5_rate(),
        cost_label(score),
        score.unreachable,
    );
}

fn cost_label(score: Score) -> String {
    if score.total == score.unreachable {
        "-".into()
    } else {
        format!("{:.2}", score.avg_cost())
    }
}

pub fn run(args: &[String]) -> i32 {
    let (Some(dict_path), Some(test_path)) = (value(args, "--dict"), value(args, "--test")) else {
        eprintln!("personalize 需要 --dict 与 --test(可选 --train、--gram、--rerank-dir)");
        return 2;
    };
    let repeats = value(args, "--repeats")
        .and_then(|text| text.parse().ok())
        .unwrap_or(1usize);
    let limit = value(args, "--limit")
        .and_then(|text| text.parse().ok())
        .unwrap_or(usize::MAX);
    if repeats == 0 {
        eprintln!("--repeats 必须大于 0");
        return 2;
    }
    let (dict, _) = match Dictionary::from_path(Path::new(dict_path)) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("词典加载失败 {dict_path}: {error}");
            return 2;
        }
    };
    let gram =
        value(args, "--gram").and_then(|path| Gram::open(Path::new(path)).ok().map(Arc::new));
    let models = match value(args, "--rerank-dir") {
        Some(dir) => match rerank::load(Path::new(dir)) {
            Ok(models) => models,
            Err(error) => {
                eprintln!("模型加载失败 {dir}: {error}");
                return 2;
            }
        },
        None => {
            eprintln!("personalize 需要 --rerank-dir(产品链路包含 8M/25M)");
            return 2;
        }
    };
    let ctx = Ctx {
        dict: Arc::new(dict),
        gram,
        models,
    };
    let test: Vec<Case> = load_cases(test_path).into_iter().take(limit).collect();
    if test.is_empty() {
        eprintln!("测试语料没有可用条目 {test_path}");
        return 2;
    }
    // `--train` 可重复出现:按命令行顺序合并多份历史日志。
    let mut profile: Vec<Case> = Vec::new();
    for (at, arg) in args.iter().enumerate() {
        if arg == "--train"
            && let Some(path) = args.get(at + 1)
        {
            profile.extend(load_cases(path));
        }
    }
    let saved = seed(&ctx, &profile);

    // 对照组:每条测试用例一个新引擎,导入同一份档案;在线不学习。
    let mut flat_by_round: Vec<Score> = vec![Score::default(); repeats];
    // 实验组:一条时间线,测完就按正确选择上屏(学习)。
    let mut primed_by_round: Vec<Score> = vec![Score::default(); repeats];
    let mut total_flips = Flips::default();
    let mut flips_by_round = vec![Flips::default(); repeats];
    let mut by_group: BTreeMap<String, (Score, Score)> = BTreeMap::new();
    let mut online = EngineSet::new(&ctx, Some(&saved), true);
    let mut day = 0u64;
    for round in 0..repeats {
        for case in &test {
            set_sim_now(BASE + day * DAY);
            day += 1;
            let flat_cost = {
                let mut set = EngineSet::new(&ctx, Some(&saved), false);
                let rank = set.measure(case);
                (rank, set.cost(case))
            };
            set_sim_now(BASE + day * DAY);
            let primed_rank = online.measure(case);
            let primed_cost = online.cost(case);
            flat_by_round[round].add(flat_cost.0, flat_cost.1);
            primed_by_round[round].add(primed_rank, primed_cost);
            total_flips.add(flat_cost.0, primed_rank);
            flips_by_round[round].add(flat_cost.0, primed_rank);
            let (unit, reading) = group_of(case.category);
            let key = format!("{unit}/{reading}");
            let entry = by_group.entry(key).or_default();
            entry.0.add(flat_cost.0, flat_cost.1);
            entry.1.add(primed_rank, primed_cost);
        }
    }
    let flat_total = {
        let mut all = Score::default();
        for score in &flat_by_round {
            all.total += score.total;
            all.top1 += score.top1;
            all.top5 += score.top5;
            all.picks += score.picks;
            all.pages += score.pages;
            all.unreachable += score.unreachable;
        }
        all
    };
    let primed_total = {
        let mut all = Score::default();
        for score in &primed_by_round {
            all.total += score.total;
            all.top1 += score.top1;
            all.top5 += score.top5;
            all.picks += score.picks;
            all.pages += score.pages;
            all.unreachable += score.unreachable;
        }
        all
    };
    let name = Path::new(test_path)
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let mut out = format!(
        "# 个性化时间序模拟({name})\n\n档案 {} 条,测试 {} 条 × {} 遍;学习关闭/开启只影响用户词库,引擎与重排门控相同。\n\n",
        profile.len(),
        test.len(),
        repeats
    );
    let _ = writeln!(
        out,
        "## 汇总\n\n| 组 | 观测 | P@1 | P@5 | 成功时平均选词+翻页 | 打不出 |\n|---|---:|---:|---:|---:|---:|"
    );
    write_score(&mut out, "flat(档案,不学测试)", flat_total);
    write_score(&mut out, "primed(档案 + 在线学习)", primed_total);
    for (i, delta) in [
        primed_total.top1_rate() - flat_total.top1_rate(),
        primed_total.top5_rate() - flat_total.top5_rate(),
    ]
    .into_iter()
    .enumerate()
    {
        let k = [1, 5][i];
        let (wins, losses) = (total_flips.improved[i], total_flips.regressed[i]);
        let _ = writeln!(
            out,
            "\n逐条配对:P@{k} 进步 {wins},退步 {losses},净 {delta:+.1} 个百分点。"
        );
    }
    let _ = writeln!(
        out,
        "\n同一档案沿时间线在线学习;跨遍复用题目。独立题数至多 {},不基于这些翻转数做显著性推断。\n",
        test.len()
    );
    let _ = writeln!(
        out,
        "## 分遍\n\n| 遍 | n | flat P@1 | primed P@1 | flat P@5 | primed P@5 | P@1 进/退 | P@5 进/退 | flat 成功成本 | primed 成功成本 |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
    );
    for round in 0..repeats {
        let flat = flat_by_round[round];
        let primed = primed_by_round[round];
        let flips = flips_by_round[round];
        let _ = writeln!(
            out,
            "| {} | {} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {}/{} | {}/{} | {} | {} |",
            round + 1,
            flat.total,
            flat.top1_rate(),
            primed.top1_rate(),
            flat.top5_rate(),
            primed.top5_rate(),
            flips.improved[0],
            flips.regressed[0],
            flips.improved[1],
            flips.regressed[1],
            cost_label(flat),
            cost_label(primed)
        );
    }
    let _ = writeln!(
        out,
        "\n## 分组(按单元/阅读形态)\n\n| 组 | n | flat P@1 | primed P@1 | Δ | flat 成功成本 | primed 成功成本 |\n|---|---:|---:|---:|---:|---:|---:|"
    );
    for (group, (flat, primed)) in &by_group {
        let _ = writeln!(
            out,
            "| {group} | {} | {:.1}% | {:.1}% | {:+.1} | {} | {} |",
            flat.total,
            flat.top1_rate(),
            primed.top1_rate(),
            primed.top1_rate() - flat.top1_rate(),
            cost_label(*flat),
            cost_label(*primed)
        );
    }
    let _ = std::fs::create_dir_all("target");
    let path = format!("target/personalize-{name}.md");
    let _ = std::fs::write(&path, &out);
    print!("{out}\n报告: {path}\n");
    0
}
