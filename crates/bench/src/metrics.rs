//! 指标 E2E:运行指标、快照往返与 `lueying ext top`。
//!
//! 失败方式:
//! 1. 指标不记录或峰值算错,`top` 看不到派发。
//! 2. 快照写读不成,设置页与 CLI 看到旧值或空值。
//! 3. 表格丢掉扩展、状态或失败原因。

use std::time::Duration;

use lueying_cli::{Invocation, TopOptions, parse_args, read_metrics, render_json, render_table};
use lueying_ext::host::{ExtState, Runtime};
use lueying_ext::manifest::metrics::{ExtMetrics, ExtStat};

use crate::extwasm::harness::{command_manifest, fixture_a, fixture_or_skip, write_pack};
use crate::harness::Temp;

pub fn run(report: &mut String) -> usize {
    let fixture = fixture_or_skip!(fixture_a());
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();

    // 1. 未加载、加载、卸载三种状态与派发指标。
    let temp = Temp::new("stats");
    let dir = temp.pack("demo");
    write_pack(
        &dir,
        &command_manifest("demo", &["run"], &["notify"]),
        &fixture,
    );
    let order = vec!["demo".to_string()];
    let (mut runtime, errors) = Runtime::open(&temp.root, &order);
    checks.push(("指标包打开", errors.is_empty(), format!("{errors:?}")));
    let idle = runtime.stats();
    checks.push((
        "未加载时显示空闲",
        idle.len() == 1 && idle[0].state == ExtState::Idle,
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
        "加载后有派发计数",
        ran.is_ok() && loaded[0].state == ExtState::Loaded && loaded[0].dispatches >= 1,
        format!("run={ran:?} stat={:?}", loaded.first()),
    ));
    runtime.deactivate("demo");
    let idle = runtime.stats();
    checks.push((
        "卸载后回到空闲",
        idle.first()
            .is_some_and(|stat| stat.state == ExtState::Idle),
        format!("stat={:?}", idle.first()),
    ));
    let gone = runtime.unload("demo") && runtime.stats().is_empty();
    checks.push((
        "移除扩展后不再列出",
        gone,
        "unload 移除槽位,指标里不应留下旧行".to_string(),
    ));

    // 2. 快照往返、设置页摘要与 CLI 表格/JSON。
    let path = temp.root.join("ext-metrics.json");
    let loaded_stat = ExtStat {
        id: "demo".to_string(),
        name: "演示".to_string(),
        version: "1.0.0".to_string(),
        state: ExtState::Loaded,
        dispatches: 5,
        dispatch_peak_us: 1500,
        pending: 1,
        last_error: None,
    };
    let mut pending_stat = loaded_stat.clone();
    pending_stat.id = "pending".to_string();
    pending_stat.state = ExtState::Unapproved;
    let metrics = ExtMetrics {
        version: lueying_ext::host::METRICS_VERSION,
        updated_ms: 42,
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

    // 3. 参数解析:正式入口是 `lueying ext top`。
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
    crate::harness::write(report, "运行可见性", checks.to_vec())
}
