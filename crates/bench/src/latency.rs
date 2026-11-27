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
use std::time::Instant;

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
///
/// 优先用 `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`:纳秒精度,单个按键的亚毫秒
/// 差异也读得出来。拿不到时退回 `/proc/thread-self/schedstat`,再退到
/// `/proc/thread-self/stat` 的时钟滴答(本机 4 毫秒一跳,只够累加整段会话)。
fn thread_cpu_ms() -> Option<f64> {
    // CLOCK_THREAD_CPUTIME_ID:本线程真正占用的 CPU,纳秒精度。`/proc` 里的两条都是
    // 按 jiffy 记账(本机 4 毫秒一跳),单个按键的亚毫秒差异会被量化掉,只够看整段会话。
    if let Some(ms) = thread_cpu_clock_ms() {
        return Some(ms);
    }
    if let Ok(sched) = std::fs::read_to_string("/proc/thread-self/schedstat")
        && let Some(ns) = sched
            .split_whitespace()
            .next()
            .and_then(|text| text.parse::<f64>().ok())
    {
        return Some(ns / 1_000_000.0);
    }
    let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    // 括号后第 1 列是 state(第 3 列),utime、stime 分别是第 14、15 列,即这里的下标 11、12。
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;
    Some((utime + stime) * 1000.0 / CLOCK_TICKS_PER_SEC)
}

/// `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`,纳秒精度;非 Linux 或调用失败时返回 `None`。
///
/// 直接声明 C 符号,不额外引入 `libc`:这一个调用是基准里唯一需要的系统接口。
fn thread_cpu_clock_ms() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        #[repr(C)]
        struct Timespec {
            secs: i64,
            nanos: i64,
        }
        unsafe extern "C" {
            fn clock_gettime(clock: i32, at: *mut Timespec) -> i32;
        }
        const CLOCK_THREAD_CPUTIME_ID: i32 = 3;
        let mut at = Timespec { secs: 0, nanos: 0 };
        // SAFETY:传的是本栈上的 `Timespec`,写满一个结构体;C 侧只读时钟 ID。
        if unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut at) } == 0 {
            return Some(at.secs as f64 * 1000.0 + at.nanos as f64 / 1e6);
        }
    }
    None
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

/// 逐键延迟:同一会话连续打多句,上屏后保留上下文,模拟真实按键路径。
///
/// 每个字母记录三段时间:引擎重算、8M 同步决策、两者合计;墙钟之外再记本线程
/// CPU 时间(推理是单线程纯 Rust,线程 CPU 时长不受邻居进程调度影响)。句间不清
/// 上下文,这样上屏后的前缀缓存与候选续跑能跨句复用;对照改前/改后时只看同一
/// 语料、同一模型与同一门控。
pub fn keys(args: &[String]) -> i32 {
    let (Some(dict_path), Some(rerank_dir)) = (value(args, "--dict"), value(args, "--rerank-dir"))
    else {
        eprintln!("keys 需要 --dict 与 --rerank-dir(可选 --gram、--corpus、--cases)");
        return 2;
    };
    let corpus_path = value(args, "--corpus").unwrap_or("dicts/eval-user-v4.tsv");
    let limit = value(args, "--cases")
        .and_then(|text| text.parse().ok())
        .unwrap_or(usize::MAX);
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
    let mut engine = bench::make_engine(&dict, &gram, Fuzzy::auto());
    engine.set_learning(false);
    if let Some(context) = cases.iter().find(|case| !case.context.is_empty()) {
        engine.set_context(context.context);
    }
    let mut four = Rerank::new(&models.four);
    let mut core_ms = Vec::new();
    let mut eight_ms = Vec::new();
    let mut total_ms = Vec::new();
    let mut core_cpu = Vec::new();
    let mut eight_cpu = Vec::new();
    let mut total_cpu = Vec::new();
    let mut rows = String::from("input\texpected\trank\tkeys\tms_engine\tms_8m\tms_total\n");
    let mut per_key =
        String::from("case\tkey\tfirst\tengine_ms\tengine_cpu\teight_ms\teight_cpu\n");
    let (mut top1, mut total) = (0usize, 0usize);
    for case in cases.iter().take(limit) {
        engine.process(Key::Esc);
        let expected = bench::normalize(case.expected);
        let mut keys = 0usize;
        let mut case_engine = 0.0;
        let mut case_eight = 0.0;
        for c in case.input.chars() {
            let cpu_before = thread_cpu_ms().unwrap_or(0.0);
            let started = Instant::now();
            engine.process(Key::Letter(c));
            let core = started.elapsed().as_secs_f64() * 1000.0;
            let cpu_after_engine = thread_cpu_ms().unwrap_or(cpu_before);
            let mut eight = 0.0;
            let snap = rerank::snapshot(&engine);
            if !snap.context.is_empty() && !snap.texts.is_empty() {
                let texts: Vec<&str> = snap.texts.iter().map(String::as_str).collect();
                let started = Instant::now();
                let promote = four.decide(&snap.context, &texts, &snap.facts);
                eight = started.elapsed().as_secs_f64() * 1000.0;
                if let Some(index) = promote {
                    engine.promote(index);
                }
            }
            let cpu_after_eight = thread_cpu_ms().unwrap_or(cpu_after_engine);
            let total = started.elapsed().as_secs_f64() * 1000.0;
            core_ms.push(core);
            eight_ms.push(eight);
            total_ms.push(total);
            core_cpu.push(cpu_after_engine - cpu_before);
            eight_cpu.push(cpu_after_eight - cpu_after_engine);
            total_cpu.push(cpu_after_eight - cpu_before);
            let _ = writeln!(
                per_key,
                "{total}\t{keys}\t{}\t{core:.3}\t{:.3}\t{eight:.3}\t{:.3}",
                usize::from(keys == 0),
                cpu_after_engine - cpu_before,
                cpu_after_eight - cpu_after_engine,
            );
            case_engine += core;
            case_eight += eight;
            keys += 1;
        }
        let rank = shared::rank_of(&engine, &expected);
        top1 += usize::from(rank == 0);
        total += 1;
        let _ = writeln!(
            rows,
            "{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}",
            case.input.replace(['\t', '\n'], " "),
            case.expected.replace(['\t', '\n'], " "),
            rank,
            keys,
            case_engine,
            case_eight,
            case_engine + case_eight
        );
        // 上屏当前首选,上下文留给下一句;前缀词没吃完时再落定剩余组合。
        engine.process(Key::Space);
        if engine.composition().is_some() {
            engine.flush();
        }
    }
    let mut out = format!(
        "# 逐键延迟({corpus_path})\n\n{} 句(坏行 {malformed}),学习关闭;句间保留已上屏上下文。\n\n| 指标 | 引擎墙钟 | 8M 墙钟 | 合计墙钟 | 引擎 CPU | 8M CPU | 合计 CPU |\n|---|---:|---:|---:|---:|---:|---:|\n",
        total
    );
    let (p50_core, p50_eight, p50_total) = (
        shared::pct(&mut core_ms.clone(), 50.0),
        shared::pct(&mut eight_ms.clone(), 50.0),
        shared::pct(&mut total_ms.clone(), 50.0),
    );
    let (p95_core, p95_eight, p95_total) = (
        shared::pct(&mut core_ms.clone(), 95.0),
        shared::pct(&mut eight_ms.clone(), 95.0),
        shared::pct(&mut total_ms.clone(), 95.0),
    );
    let (p99_core, p99_eight, p99_total) = (
        shared::pct(&mut core_ms.clone(), 99.0),
        shared::pct(&mut eight_ms.clone(), 99.0),
        shared::pct(&mut total_ms.clone(), 99.0),
    );
    let (c50_core, c50_eight, c50_total) = (
        shared::pct(&mut core_cpu, 50.0),
        shared::pct(&mut eight_cpu, 50.0),
        shared::pct(&mut total_cpu, 50.0),
    );
    let (c95_core, c95_eight, c95_total) = (
        shared::pct(&mut core_cpu, 95.0),
        shared::pct(&mut eight_cpu, 95.0),
        shared::pct(&mut total_cpu, 95.0),
    );
    let (c99_core, c99_eight, c99_total) = (
        shared::pct(&mut core_cpu, 99.0),
        shared::pct(&mut eight_cpu, 99.0),
        shared::pct(&mut total_cpu, 99.0),
    );
    let total_wall: f64 = total_ms.iter().sum();
    let _ = writeln!(
        out,
        "| p50 | {p50_core:.2} | {p50_eight:.2} | {p50_total:.2} | {c50_core:.2} | {c50_eight:.2} | {c50_total:.2} |\n| p95 | {p95_core:.2} | {p95_eight:.2} | {p95_total:.2} | {c95_core:.2} | {c95_eight:.2} | {c95_total:.2} |\n| p99 | {p99_core:.2} | {p99_eight:.2} | {p99_total:.2} | {c99_core:.2} | {c99_eight:.2} | {c99_total:.2} |\n\n按键 {} 个,P@1 {}/{} = {:.1}%。合计墙钟 {total_wall:.1} ms。",
        total_ms.len(),
        top1,
        total,
        100.0 * top1 as f64 / total.max(1) as f64,
    );
    let _ = std::fs::create_dir_all("target/bench");
    let _ = std::fs::write("target/bench/keys.tsv", rows);
    let _ = std::fs::write("target/bench/keys-perkey.tsv", per_key);
    let _ = std::fs::write("target/keys.md", &out);
    let total_cpu: f64 = total_cpu.iter().sum::<f64>().max(0.0);
    println!(
        "keys-summary cases={total} keys={} top1={top1} wall-total-ms={total_wall:.1} cpu-total-ms={total_cpu:.1}",
        total_ms.len()
    );
    print!("{out}\n逐条结果: target/bench/keys.tsv / keys-perkey.tsv\n报告: target/keys.md\n");
    0
}

/// 前缀续算的触发面:同一条上下文,上屏(上下文变长)之后的那一次决策,冷启动重跑
/// 整段前缀 vs 会话里已有前缀续算,各花多少本线程 CPU。
///
/// 按键模型的窗口是 `context_chars` 字;上下文超过窗口后每次上屏都会左移窗口,
/// 新旧 token 序列不再有前缀关系,续算无从触发。这个探针把边界测出来:
/// `L + commit <= 窗口` 的行才该看到续算收益。
///
/// 用法:`lueying-bench prefix --rerank-dir <模型目录> [--corpus <语料>] [--samples 15]`
pub fn prefix(args: &[String]) -> i32 {
    let Some(rerank_dir) = value(args, "--rerank-dir") else {
        eprintln!("prefix 需要 --rerank-dir(可选 --corpus、--samples)");
        return 2;
    };
    let corpus_path = value(args, "--corpus").unwrap_or("dicts/eval-dev-v4.tsv");
    let samples = value(args, "--samples")
        .and_then(|text| text.parse().ok())
        .unwrap_or(15);
    let Ok(models) = rerank::load(Path::new(rerank_dir)) else {
        eprintln!("模型加载失败 {rerank_dir}");
        return 2;
    };
    let window = std::fs::read_to_string(Path::new(rerank_dir).join("rerank-8m.model.json"))
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|json| json.get("context_chars").and_then(|v| v.as_u64()))
        .map_or(0, |v| v as usize);
    let Ok(body) = std::fs::read_to_string(corpus_path) else {
        eprintln!("读语料失败 {corpus_path}");
        return 2;
    };
    let leaked: &'static str = Box::leak(body.into_boxed_str());
    let (cases, _) = shared::cases(leaked);
    let mut text = String::new();
    for case in &cases {
        let expected = bench::normalize(case.expected);
        text.push_str(&expected);
        if text.chars().count() >= 200 {
            break;
        }
    }
    let chars: Vec<char> = text.chars().collect();
    let commit = 6usize;
    let words: Vec<String> = cases
        .iter()
        .map(|case| bench::normalize(case.expected))
        .filter(|word| word.chars().count() == 4)
        .take(2)
        .collect();
    if words.len() < 2 || chars.len() < 140 {
        eprintln!("语料太短,凑不出探针用的上下文与候选");
        return 2;
    }
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let lengths = [8usize, 16, 24, 32, 40, 48, 56, 58, 60, 64, 72, 88, 104, 120];
    let mut out = String::from(
        "## 上屏后的前缀续算(8M,线程 CPU 中位数)\n\n| 前缀 L | 上屏后 L+6 | 窗口内 | 冷启动 ms | 续算 ms | 续算/冷启动 |\n|---:|---:|:---:|---:|---:|---:|\n",
    );
    let mut last_cold = 0.0;
    let mut last_warm = 0.0;
    for length in lengths {
        if length + commit > chars.len() {
            break;
        }
        let context: String = chars[..length].iter().collect();
        let target: String = chars[..length + commit].iter().collect();
        let mut cold = Vec::with_capacity(samples);
        let mut warm = Vec::with_capacity(samples);
        for _ in 0..samples {
            let started = thread_cpu_ms().unwrap_or(0.0);
            let mut rerank = Rerank::new(&models.four);
            rerank.scores(&target, &words);
            let done = thread_cpu_ms().unwrap_or(started);
            cold.push(done - started);

            let mut rerank = Rerank::new(&models.four);
            rerank.scores(&context, &words);
            let started = thread_cpu_ms().unwrap_or(0.0);
            rerank.scores(&target, &words);
            let done = thread_cpu_ms().unwrap_or(started);
            warm.push(done - started);
        }
        let cold_ms = median(&mut cold);
        let warm_ms = median(&mut warm);
        last_cold = cold_ms;
        last_warm = warm_ms;
        let inside = window > 0 && length + commit <= window;
        let _ = writeln!(
            out,
            "| {length} | {} | {} | {cold_ms:.3} | {warm_ms:.3} | {:.3} |",
            length + commit,
            if inside { "是" } else { "否" },
            warm_ms / cold_ms.max(1e-9)
        );
    }
    println!(
        "prefix-summary last-cold-ms={last_cold:.3} last-warm-ms={last_warm:.3} window={window} commit={commit}"
    );
    print!("{out}");
    0
}
