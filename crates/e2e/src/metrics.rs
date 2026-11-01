//! 预算与可见性 E2E:运行指标、超限停用、快照往返与 `lueying ext top`。
//!
//! 失败方式:
//! 1. 指标不记录或峰值算错,`top` 看不到超限。
//! 2. 超预算的扩展没有停用,坏扩展继续拖慢宿主。
//! 3. 快照写读不成,设置页与 CLI 看到旧值或空值。
//! 4. 表格丢掉扩展、状态或超限次数。

use std::time::Duration;

use lueying_cli::{Invocation, TopOptions, parse_args, read_metrics, render_json, render_table};
use lueying_extwasm::{ExtState, Limits, MAX_FAULTS, Pack, Runtime};
use lueying_extmeta::metrics::{Budgets, ExtMetrics, ExtStat};

use crate::extwasm_harness::{command_guest, command_manifest, loop_guest, write_pack};
use crate::harness::Temp;

const GOOD: &str = r#"{"ok":true}"#;

pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();

    // 1. 未加载、加载、卸载三种状态与内存/耗时指标。
    let temp = Temp::new("stats");
    let dir = temp.pack("demo");
    write_pack(
        &dir,
        &command_manifest("demo", &["run"], &[]),
        &command_guest(GOOD),
    );
    let order = vec!["demo".to_string()];
    let (mut runtime, errors) = Runtime::open(&temp.root, &order, Limits::default());
    checks.push(("指标包打开", errors.is_empty(), format!("{errors:?}")));
    let idle = runtime.stats();
    checks.push((
        "未加载时显示空闲与预算",
        idle.len() == 1
            && idle[0].state == ExtState::Idle
            && idle[0].memory_used == 0
            && idle[0].memory_limit == Limits::default().memory_bytes as u64,
        format!("{idle:?}"),
    ));
    let ran = runtime.invoke("demo.run", "{}");
    loop {
        if !runtime.take_results().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let loaded = runtime.stats();
    checks.push((
        "加载后有内存与派发计数",
        ran.is_ok()
            && loaded[0].state == ExtState::Loaded
            && loaded[0].dispatches >= 1
            && loaded[0].memory_used > 0,
        format!("run={ran:?} stat={:?}", loaded.first()),
    ));
    let peak = loaded[0].dispatch_peak_us;
    let budget_us = Limits::default().dispatch_ms * 1000;
    // CI 延迟门禁:一次普通命令的峰值必须落在派发预算内。
    checks.push((
        "CI 延迟门禁",
        peak <= budget_us,
        format!("派发峰值 {peak} us / 预算 {budget_us} us"),
    ));
    runtime.deactivate("demo");
    let idle = runtime.stats();
    checks.push((
        "卸载后回到空闲",
        idle.first()
            .is_some_and(|stat| stat.state == ExtState::Idle && stat.memory_used == 0),
        format!("stat={:?}", idle.first()),
    ));
    let gone = runtime.unload("demo") && runtime.stats().is_empty();
    checks.push((
        "移除扩展后不再列出",
        gone,
        "unload 移除槽位,指标里不应留下旧行".to_string(),
    ));

    // 2. 死循环连续超限后停用;状态与超限次数都要能看见。
    let spin_temp = Temp::new("spin");
    let dir = spin_temp.pack("demo");
    write_pack(
        &dir,
        &command_manifest("demo", &["run"], &[]),
        &loop_guest(),
    );
    let limits = Limits {
        command_fuel: 1_000_000,
        ..Limits::default()
    };
    let mut pack = match Pack::load_with(&dir, limits) {
        Ok(pack) => pack,
        Err(error) => {
            checks.push(("加载死循环包", false, error));
            return write(report, &checks);
        }
    };
    for _ in 0..MAX_FAULTS {
        let _ = pack.invoke_wait("run", "{}", Duration::from_secs(30));
    }
    let stat = pack.stat();
    let stopped = pack.invoke_wait("run", "{}", Duration::from_secs(30));
    checks.push((
        "连续超限停用",
        stat.state == ExtState::Disabled
            && stat.faults >= MAX_FAULTS
            && stat.timeouts >= u64::from(MAX_FAULTS)
            && stopped.is_err_and(|error| error.contains("已停用")),
        format!(
            "state={:?} faults={} timeouts={} stopped={stopped:?}",
            stat.state, stat.faults, stat.timeouts
        ),
    ));

    // 3. 快照往返、设置页摘要与 CLI 表格/JSON。
    let path = temp.root.join("ext-metrics.json");
    let loaded_stat = ExtStat {
        id: "demo".to_string(),
        name: "演示".to_string(),
        version: "1.0.0".to_string(),
        state: ExtState::Loaded,
        memory_used: 1 << 20,
        memory_limit: 16 << 20,
        dispatches: 5,
        steps: 2,
        dispatch_peak_us: 700,
        step_peak_us: 1500,
        timeouts: 0,
        ooms: 0,
        faults: 0,
        pending: 1,
        last_error: None,
    };
    let mut pending_stat = loaded_stat.clone();
    pending_stat.id = "pending".to_string();
    pending_stat.state = ExtState::Unapproved;
    let metrics = ExtMetrics {
        version: lueying_extwasm::METRICS_VERSION,
        updated_ms: 42,
        budgets: Budgets {
            memory_bytes: 16 << 20,
            stack_bytes: 256 << 10,
            init_ms: 100,
            dispatch_ms: 100,
            max_faults: MAX_FAULTS,
        },
        extensions: vec![loaded_stat, pending_stat],
    };
    metrics.save(&path).expect("保存快照");
    let loaded = read_metrics(&path);
    let table = loaded.as_ref().map(render_table).unwrap_or_default();
    let summary = loaded.as_ref().map(ExtMetrics::summary).unwrap_or_default();
    let json = loaded
        .as_ref()
        .ok()
        .and_then(|metrics| render_json(metrics).ok());
    checks.push((
        "快照往返与 CLI 表格",
        loaded.as_ref().is_ok_and(|loaded| loaded == &metrics)
            && table.contains("demo")
            && table.contains("运行")
            && table.contains("待同意")
            && table.contains("1.5 ms")
            && summary.contains("运行 1")
            && summary.contains("待同意 1"),
        format!("loaded={loaded:?} table={table:?}"),
    ));
    checks.push((
        "CLI JSON 可解析",
        json.as_deref()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .is_some_and(|value| value["extensions"][0]["id"] == "demo"),
        format!("{json:?}"),
    ));
    checks.push((
        "CLI 缺文件报错",
        read_metrics(&temp.root.join("missing.json")).is_err(),
        "缺快照应给出可操作错误".to_string(),
    ));

    // 4. 参数解析:正式入口是 `lueying ext top`。
    let file = temp.root.join("custom.json");
    let parsed = parse_args([
        "ext".to_string(),
        "top".to_string(),
        "--file".to_string(),
        file.display().to_string(),
        "--json".to_string(),
    ]);
    checks.push((
        "CLI 参数解析",
        parsed
            == Ok(Invocation::Top(TopOptions {
                file: Some(file),
                json: true,
                watch: false,
            }))
            && parse_args(["ext".to_string(), "nope".to_string()]).is_err(),
        format!("{parsed:?}"),
    ));

    write(report, &checks)
}

fn write(report: &mut String, checks: &[(&'static str, bool, String)]) -> usize {
    crate::harness::write(report, "预算与运行可见性", checks.to_vec())
}
