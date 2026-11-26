//! 个性化留出评估:用前几次会话学到的用户词表,测后面一次会话的首选与按键成本。
//!
//! 与 `learn-sim` 的区别:
//! - `learn-sim` 在用户自己的句子上边测边学(第 1/2/3 遍),衡量同一批句子的收敛;
//! - 这里把 `eval-user-v1..v3` 当历史日志训练,把 `eval-user-v4` 当留出会话评测,
//!   报告无个性化(flat)与导入用户词表后(primed)的 P@1/P@5、键/字、选词与翻页。
//!
//! 键/字沿用产品按键路径:字母数固定,选词按数字键、翻页按 PageDown 计一键,
//! 所以个性化只能通过减少选词/翻页来省键。`--side-effect` 额外在 `eval-dev-v4`
//! 上测同一份用户词表对通用语料的影响(引擎原序,不跑重排,快)。
//!
//! 用法:
//! `lueying-bench personal --dict <主词典> [--gram <搭配表>] [--rerank-dir <模型目录>]
//!  [--passes 1] [--side-effect]`

use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use lueying_core::{Engine, Fuzzy, Key, set_sim_now};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::learn_sim::learn_sentence;
use crate::rerank::{self, Models};
use crate::shared::{self, Case};

const DAY: u64 = 86_400;
const BASE_SECS: u64 = 1_800_000_000;
const TRAIN_DIR: &str = "dicts";
const TEST: &str = "dicts/eval-user-v4.tsv";
const DEV: &str = "dicts/eval-dev-v4.tsv";

struct Ctx {
    dict: Arc<Dictionary>,
    gram: Option<Arc<Gram>>,
    models: Option<Models>,
}

impl Ctx {
    fn engine(&self, learning: bool) -> Engine {
        let mut engine = bench::make_engine(&self.dict, &self.gram, Fuzzy::auto());
        engine.set_learning(learning);
        engine
    }
}

#[derive(Default)]
struct Row {
    rank: i64,
    rank_settled: i64,
    keys: f64,
    chars: f64,
    picks: i64,
    pages: i64,
    stuck: bool,
    ms: Vec<f64>,
}

#[derive(Default)]
struct Total {
    rows: Vec<Row>,
}

impl Total {
    fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    fn top1(&self) -> f64 {
        pct(
            self.rows.iter().filter(|r| r.rank == 0).count(),
            self.rows.len(),
        )
    }

    fn top5(&self) -> f64 {
        pct(
            self.rows
                .iter()
                .filter(|r| (0..5).contains(&r.rank))
                .count(),
            self.rows.len(),
        )
    }

    fn settled_top1(&self) -> f64 {
        pct(
            self.rows.iter().filter(|r| r.rank_settled == 0).count(),
            self.rows.len(),
        )
    }

    fn settled_top5(&self) -> f64 {
        pct(
            self.rows
                .iter()
                .filter(|r| (0..5).contains(&r.rank_settled))
                .count(),
            self.rows.len(),
        )
    }

    fn keys_per_char(&self) -> f64 {
        let done: Vec<&Row> = self.rows.iter().filter(|r| !r.stuck).collect();
        let keys: f64 = done.iter().map(|r| r.keys).sum();
        let chars: f64 = done.iter().map(|r| r.chars).sum();
        if chars <= 0.0 { 0.0 } else { keys / chars }
    }

    fn picks_per_sentence(&self) -> f64 {
        let done: Vec<&Row> = self.rows.iter().filter(|r| !r.stuck).collect();
        if done.is_empty() {
            0.0
        } else {
            done.iter().map(|r| r.picks as f64).sum::<f64>() / done.len() as f64
        }
    }

    fn pages_per_sentence(&self) -> f64 {
        let done: Vec<&Row> = self.rows.iter().filter(|r| !r.stuck).collect();
        if done.is_empty() {
            0.0
        } else {
            done.iter().map(|r| r.pages as f64).sum::<f64>() / done.len() as f64
        }
    }

    fn stuck_pct(&self) -> f64 {
        pct(
            self.rows.iter().filter(|r| r.stuck).count(),
            self.rows.len(),
        )
    }

    fn latency(&self) -> (f64, f64, f64) {
        let mut values: Vec<f64> = self
            .rows
            .iter()
            .flat_map(|r| r.ms.iter().copied())
            .collect();
        (
            shared::pct(&mut values, 50.0),
            shared::pct(&mut values, 95.0),
            shared::pct(&mut values, 99.0),
        )
    }
}

fn pct(part: usize, total: usize) -> f64 {
    100.0 * part as f64 / total.max(1) as f64
}

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

fn read_cases(path: &Path) -> Vec<Case<'static>> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let body: &'static str = Box::leak(text.into_boxed_str());
    shared::cases(body).0
}

fn user_sentences(dir: &str, test: bool) -> Vec<Case<'static>> {
    if test {
        return read_cases(Path::new(TEST));
    }
    let mut out = Vec::new();
    for n in 1..=3 {
        let path = format!("{dir}/eval-user-v{n}.tsv");
        out.extend(read_cases(Path::new(&path)));
    }
    out
}

/// 走产品按键路径上屏整句:选能接上剩余文字的最长候选,必要时翻页。
/// 返回选词与翻页次数;打不出来时选词次数为 -1。
fn eval_case(
    engine: &mut Engine,
    case: &Case,
    four: &mut Option<Rerank>,
    five: &mut Option<Rerank>,
) -> Row {
    engine.process(Key::Esc);
    if case.context.is_empty() {
        engine.clear_context();
    } else {
        engine.set_context(case.context);
    }
    let mut ms = Vec::new();
    let letters = case.input.chars().filter(char::is_ascii_alphabetic).count() as f64;
    for c in case.input.chars() {
        let started = Instant::now();
        engine.process(Key::Letter(c));
        ms.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    let expected = bench::normalize(case.expected);
    let rank = shared::rank_of(engine, &expected);
    let rank_settled = match (four.as_mut(), five.as_mut()) {
        (Some(four), Some(five)) => {
            shared::apply_rerank(engine, four, five, &expected);
            shared::rank_of(engine, &expected)
        }
        _ => rank,
    };
    let (picks, pages) = shared::input_cost(engine, &expected);
    let stuck = picks < 0;
    let keys = if stuck {
        letters
    } else {
        letters + picks as f64 + pages as f64
    };
    Row {
        rank,
        rank_settled,
        keys,
        chars: expected.chars().count() as f64,
        picks: picks.max(0),
        pages,
        stuck,
        ms,
    }
}

fn train(ctx: &Ctx, sentences: &[Case], passes: usize) -> (String, usize, f64, f64) {
    let mut engine = ctx.engine(true);
    let mut picks = 0usize;
    let (mut letters, mut chars) = (0.0, 0.0);
    for pass in 0..passes {
        for (index, sentence) in sentences.iter().enumerate() {
            set_sim_now(BASE_SECS + pass as u64 * DAY + index as u64 * 120);
            picks += learn_sentence(&mut engine, sentence);
            letters += sentence
                .input
                .chars()
                .filter(char::is_ascii_alphabetic)
                .count() as f64;
            chars += sentence.expected.chars().count() as f64;
        }
    }
    (engine.export_user(), picks, letters, chars)
}

pub fn run(args: &[String]) -> i32 {
    let Some(dict_path) = value(args, "--dict") else {
        eprintln!("personal 需要 --dict(可选 --gram、--rerank-dir、--passes、--side-effect)");
        return 2;
    };
    let passes = value(args, "--passes")
        .and_then(|text| text.parse().ok())
        .unwrap_or(1usize)
        .max(1);
    let (dict, _) = match Dictionary::from_path(Path::new(dict_path)) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("词典加载失败 {dict_path}: {error}");
            return 2;
        }
    };
    let gram =
        value(args, "--gram").and_then(|path| Gram::open(Path::new(path)).ok().map(Arc::new));
    let models = value(args, "--rerank-dir").and_then(|dir| match rerank::load(Path::new(dir)) {
        Ok(models) => Some(models),
        Err(error) => {
            eprintln!("模型加载失败 {dir}: {error}");
            None
        }
    });
    let ctx = Ctx {
        dict: Arc::new(dict),
        gram,
        models,
    };
    let dir = value(args, "--dicts").unwrap_or(TRAIN_DIR);
    let train_cases = user_sentences(dir, false);
    let test_cases = user_sentences(dir, true);
    if train_cases.is_empty() || test_cases.is_empty() {
        eprintln!("缺少用户语料 {dir}/eval-user-v1..v4");
        return 2;
    }
    let (saved, train_picks, train_letters, train_chars) = train(&ctx, &train_cases, passes);

    // flat:不导入任何用户词表。primed:导入训练出的词表,两边学习都关掉。
    let mut flat = ctx.engine(false);
    let mut primed = ctx.engine(false);
    primed.import_user(&saved);
    let mut four = ctx.models.as_ref().map(|models| Rerank::new(&models.four));
    let mut five = ctx.models.as_ref().map(|models| Rerank::new(&models.five));
    let mut flat_total = Total::default();
    let mut primed_total = Total::default();
    for case in &test_cases {
        flat_total.push(eval_case(&mut flat, case, &mut four, &mut five));
        primed_total.push(eval_case(&mut primed, case, &mut four, &mut five));
    }
    // 预热后重按一遍,延迟只报第二轮,不受首次页表与图表缓存影响。
    flat_total.rows.clear();
    primed_total.rows.clear();
    for case in &test_cases {
        flat_total.push(eval_case(&mut flat, case, &mut four, &mut five));
        primed_total.push(eval_case(&mut primed, case, &mut four, &mut five));
    }

    let mut out = String::from("## 个性化留出评估\n\n");
    let _ = writeln!(
        out,
        "训练:`{dir}/eval-user-v1..v3` {} 句 × {passes} 遍;验收集:`{TEST}` {} 句。学习在训练时开,评测时关。\n",
        train_cases.len(),
        test_cases.len()
    );
    let _ = writeln!(
        out,
        "训练成本:选词 {train_picks} 次,键入字母 {train_letters:.0},上屏 {train_chars:.0} 字,键/字 {:.3}。\n",
        (train_letters + train_picks as f64) / train_chars.max(1.0)
    );
    let rerank = if ctx.models.is_some() {
        "（含 8M+25M 重排）"
    } else {
        "（引擎原序）"
    };
    let _ = writeln!(
        out,
        "| 方案{rerank} | P@1 | P@5 | 定稿 P@1 | 定稿 P@5 | 键/字 | 选词/句 | 翻页/句 | 打不出 |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|"
    );
    for (name, total) in [
        ("无个性化(flat)", &flat_total),
        ("导入用户词表(primed)", &primed_total),
    ] {
        let _ = writeln!(
            out,
            "| {name} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.3} | {:.2} | {:.2} | {:.1}% |",
            total.top1(),
            total.top5(),
            total.settled_top1(),
            total.settled_top5(),
            total.keys_per_char(),
            total.picks_per_sentence(),
            total.pages_per_sentence(),
            total.stuck_pct(),
        );
    }
    let saved_keys = (flat_total.keys_per_char() - primed_total.keys_per_char())
        / flat_total.keys_per_char().max(1e-9)
        * 100.0;
    let (fp50, fp95, fp99) = flat_total.latency();
    let (pp50, pp95, pp99) = primed_total.latency();
    let _ = writeln!(
        out,
        "\n留出会话的按键节省(primed 相对 flat): **{saved_keys:+.2}%**。\n"
    );
    let _ = writeln!(
        out,
        "### 每键延迟(评测第二轮,毫秒)\n\n| 方案 | p50 | p95 | p99 |\n|---|---:|---:|---:|\n| flat | {fp50:.2} | {fp95:.2} | {fp99:.2} |\n| primed | {pp50:.2} | {pp95:.2} | {pp99:.2} |\n"
    );

    if args.iter().any(|a| a == "--side-effect") {
        let dev = read_cases(Path::new(DEV));
        let mut no_user = ctx.engine(false);
        let mut user = ctx.engine(false);
        user.import_user(&saved);
        // 副作用只看引擎原序:重排对两侧相同,跑 7200 次 25M 只增加噪声与时间。
        let mut none_a: Option<Rerank> = None;
        let mut none_b: Option<Rerank> = None;
        let mut base = Vec::with_capacity(dev.len());
        let mut after = Vec::with_capacity(dev.len());
        for case in &dev {
            let row = eval_case(&mut no_user, case, &mut none_a, &mut none_b);
            base.push(row.rank);
            let row = eval_case(&mut user, case, &mut none_a, &mut none_b);
            after.push(row.rank);
        }
        let top1 = |rows: &[i64]| pct(rows.iter().filter(|&&r| r == 0).count(), rows.len());
        let losses = base
            .iter()
            .zip(&after)
            .filter(|(b, a)| **b == 0 && **a != 0)
            .count();
        let wins = base
            .iter()
            .zip(&after)
            .filter(|(b, a)| **b != 0 && **a == 0)
            .count();
        let _ = writeln!(
            out,
            "### 副作用(通用验收集 `{DEV}`,n={},引擎原序)\n\n| 方案 | P@1 | 退步 | 进步 |\n|---|---:|---:|---:|\n| 无个性化 | {:.2}% | 0 | 0 |\n| 导入用户词表 | {:.2}% | {losses} | {wins} |\n",
            dev.len(),
            top1(&base),
            top1(&after),
        );
    }

    print!("{out}");
    if std::fs::create_dir_all("target").is_ok() {
        let _ = std::fs::write("target/personal.md", &out);
    }
    0
}
