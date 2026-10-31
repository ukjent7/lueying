//! Wasm 层原型测量:编译、实例化与调用的嵌入成本。
//!
//! 「Wasm 能不能当默认第三方形态」的决策数据就出自这里:冷启动链
//! (引擎创建 → 探针编译 → 实例化 → 首调)与单次调用开销,数字进 CI
//! 报告。语义矩阵(燃料停用、内存上限、坏源码)在 crate 单测,这里只
//! 做成本测量与冒烟;旁路进程的冷启动在 `lueying-bypass` 单测输出。

use std::sync::Arc;
use std::time::{Duration, Instant};

use lueying_extwasm::{CALL_FUEL, PROBE_WAT, WasmHost, WasmModule};

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
    checks.push((name, ok, note));
}

fn micros(started: Instant) -> u128 {
    started.elapsed().as_micros()
}

pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();

    // 冷启动链逐段计时;每段都是首次成本的组成部分。
    let started = Instant::now();
    let host = Arc::new(WasmHost::new().expect("引擎应能创建"));
    let engine_us = micros(started);

    let started = Instant::now();
    let module = WasmModule::compile(&host, PROBE_WAT.as_bytes()).expect("探针应编译");
    let compile_us = micros(started);

    let started = Instant::now();
    let mut probe = module.instantiate("add").expect("探针应实例化");
    let instantiate_us = micros(started);

    let started = Instant::now();
    let first = probe.call(2, 3).expect("首调应成功");
    let first_call_us = micros(started);

    // 稳态调用:1000 次取均值与峰值。
    let started = Instant::now();
    let mut peak_us: u128 = 0;
    let mut sum: u64 = 0;
    for index in 0..1000u64 {
        let begin = Instant::now();
        let value = probe.call((index % 7) as u32, 3).expect("稳态调用应成功");
        assert_eq!(value, (index % 7) + 3);
        peak_us = peak_us.max(micros(begin));
        sum += probe.stat().fuel_used;
    }
    let steady_us = micros(started) / 1000;
    let fuel_mean = sum / 1000;
    let _ = first;

    report.push_str("\n## Wasm 原型测量(探针组件,Wasmtime cranelift)\n\n");
    report.push_str("| 阶段 | 耗时 |\n|---|---|\n");
    report.push_str(&format!(
        "| 引擎创建 | {:.2} ms |\n",
        engine_us as f64 / 1000.0
    ));
    report.push_str(&format!(
        "| 探针编译(WAT→组件) | {:.2} ms |\n",
        compile_us as f64 / 1000.0
    ));
    report.push_str(&format!(
        "| 实例化 | {:.2} ms |\n",
        instantiate_us as f64 / 1000.0
    ));
    report.push_str(&format!("| 首次调用 | {:.1} µs |\n", first_call_us as f64));
    report.push_str(&format!(
        "| 稳态调用均值(1000 次) | {:.1} µs(峰值 {peak_us} µs) |\n",
        steady_us as f64
    ));
    report.push_str(&format!(
        "| 单次燃料消耗均值 | {fuel_mean} / {CALL_FUEL} |\n"
    ));

    // 冒烟:测量之外确认宿主语义没有在测量路径上退化。
    push(
        &mut checks,
        "探针往返",
        probe.call(40, 2) == Ok(42),
        format!("{:?}", probe.stat()),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    push(
        &mut checks,
        "稳态调用峰值受预算约束",
        Duration::from_micros(peak_us as u64) < Duration::from_millis(5)
            && Instant::now() < deadline,
        format!("峰值 {peak_us} µs"),
    );

    report.push_str("\n### Wasm 原型语义\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0;
    for (name, ok, note) in &checks {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if *ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}
