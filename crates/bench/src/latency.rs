//! 延迟 ABAB 对比:新旧两个基准二进制交替跑同一段产品链路,比较线程 CPU 时间。
//!
//! 墙钟时间会被邻居进程干扰;线程 CPU 时间只计本线程,交替运行让系统漂移落在两边,比值取中位数。
//! 被测的一次运行是内部子命令 `latency-probe`,打印 `cpu-ms=<数值>`,两个二进制都必须带它
//! (CI 用当前基准代码配父提交的引擎编旧版本)。CPU 时间取自 `/proc/thread-self/stat`,
//! 精度为一个时钟滴答(10 毫秒,USER_HZ=100),只支持 Linux。
//!
//! 用法:`lueying-bench latency --old <旧> --new <新> --dict .. --rerank-dir .. [--gram ..] [--rounds 6] [--cases 300]`

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use lueying_core::{Fuzzy, Key};
use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Rerank;

use crate::bench;
use crate::rerank;
use crate::shared;

const DEV: &str = include_str!("../../../dicts/eval-dev-v4.tsv");
const CLOCK_TICKS_PER_SEC: f64 = 100.0;
const DEFAULT_ROUNDS: usize = 6;
const DEFAULT_CASES: usize = 300;

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
        .map(String::as_str)
}

/// 本线程到目前为止的 CPU 时间(毫秒)。
fn thread_cpu_ms() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    // 括号后第 1 列是 state(第 3 列),utime、stime 分别是第 14、15 列,即这里的下标 11、12。
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) * 1000.0 / CLOCK_TICKS_PER_SEC)
}

/// 被测的一次运行:加载模型之后,逐条跑产品链路(8M 与 25M 两档重排),报告这段的线程 CPU 时间。
pub fn probe(args: &[String]) -> i32 {
    let (Some(dict_path), Some(rerank_dir)) = (value(args, "--dict"), value(args, "--rerank-dir"))
    else {
        eprintln!("latency-probe 需要 --dict 与 --rerank-dir");
        return 2;
    };
    let cases = value(args, "--cases")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CASES);
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
    let dict = Arc::new(dict);
    let mut engine = bench::make_engine(&dict, &gram, Fuzzy::auto());
    engine.set_learning(false);
    let mut four = Rerank::new(&models.four);
    let mut five = Rerank::new(&models.five);
    let picked: Vec<_> = shared::cases(DEV).0.into_iter().take(cases).collect();
    let Some(start) = thread_cpu_ms() else {
        eprintln!("本平台没有 /proc/thread-self/stat,无法计线程 CPU 时间");
        return 2;
    };
    for case in &picked {
        engine.process(Key::Esc);
        if case.context.is_empty() {
            engine.clear_context();
        } else {
            engine.set_context(case.context);
        }
        for c in case.input.chars() {
            engine.process(Key::Letter(c));
        }
        let expected = bench::normalize(case.expected);
        shared::apply_rerank(&mut engine, &mut four, &mut five, &expected);
    }
    let end = thread_cpu_ms().unwrap_or(start);
    println!("cpu-ms={:.1}", end - start);
    0
}

/// 跑一次被测进程,取出它报告的 CPU 时间。
fn measure(bin: &str, args: &[String]) -> Result<f64, String> {
    let mut command = Command::new(bin);
    command.arg("latency-probe");
    for flag in ["--dict", "--gram", "--rerank-dir", "--cases"] {
        if let Some(text) = value(args, flag) {
            command.args([flag, text]);
        }
    }
    let output = command
        .output()
        .map_err(|error| format!("{bin}: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("cpu-ms=")?.parse().ok())
        .ok_or_else(|| {
            format!(
                "{bin} 没有输出 cpu-ms:{}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

/// 驱动:旧、新交替各跑 `rounds` 轮,报告中位数与逐轮比值(新 / 旧)。
pub fn run(args: &[String]) -> i32 {
    let (Some(old), Some(new)) = (value(args, "--old"), value(args, "--new")) else {
        eprintln!("latency 需要 --old 与 --new 两个基准二进制");
        return 2;
    };
    let rounds = value(args, "--rounds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_ROUNDS);
    let (mut old_ms, mut new_ms) = (Vec::new(), Vec::new());
    for _ in 0..rounds {
        for (bin, sink) in [(old, &mut old_ms), (new, &mut new_ms)] {
            match measure(bin, args) {
                Ok(ms) => sink.push(ms),
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            }
        }
    }
    let ratios: Vec<f64> = old_ms.iter().zip(&new_ms).map(|(o, n)| n / o).collect();
    let (old_median, new_median) = (median(&mut old_ms), median(&mut new_ms));
    let ratio = median(&mut ratios.clone());
    let (min, max) = ratios
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), r| (lo.min(*r), hi.max(*r)));
    let mut out = String::from(
        "## 延迟 ABAB(线程 CPU 时间,新旧交替)\n\n| 轮数 | 旧 中位数 | 新 中位数 | 比值(新/旧)中位数 | 逐轮比值范围 |\n|---|---:|---:|---:|---|\n",
    );
    let _ = writeln!(
        out,
        "| {rounds} | {old_median:.1} ms | {new_median:.1} ms | {ratio:.3} | {min:.3} – {max:.3} |"
    );
    print!("{out}");
    if std::fs::create_dir_all("target").is_ok() {
        let _ = std::fs::write("target/latency.md", &out);
    }
    0
}
