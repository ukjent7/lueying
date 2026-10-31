//! 预算与可见性 E2E:运行指标、超限停用、快照往返与 `lueying ext top`。
//!
//! 失败方式:
//! 1. 指标不记录或峰值算错,`top` 看不到超限。
//! 2. 超预算脚本没有停用,坏扩展继续拖慢宿主。
//! 3. 快照写读不成,设置页与 CLI 看到旧值或空值。
//! 4. 表格丢掉扩展、状态或超限次数。

use lueying_cli::{Invocation, TopOptions, parse_args, read_metrics, render_json, render_table};
use lueying_extjs::{ExtMetrics, ExtState, Invoke, Limits, Pack, Runtime};
use lueying_extmeta::metrics::{Budgets, ExtStat};

use crate::extjs_harness::write_pack;
use crate::harness::Temp;

const MANIFEST: &str = r#"{
  "id":"demo","name":"演示","version":"1.0.0","api":2,
  "activate":{"commands":[{"id":"run","title":"运行"},{"id":"spin","title":"转"}]}
}"#;

const SCRIPT: &str = r#"
lueying.command("run", function () { return { ok: true }; });
lueying.command("spin", function () { while (true) {} });
"#;

const JOB_SCRIPT: &str = r#"
lueying.command("run", function () {
  Promise.resolve().then(function () { while (true) {} });
  return new Promise(function () {});
});
"#;

fn invoke(pack: &mut Pack, command: &str) -> String {
    match pack.invoke(command, "{}") {
        Ok(Invoke::Done(text)) => text,
        other => format!("{other:?}"),
    }
}

pub fn run(report: &mut String) -> usize {
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();

    // 1. 未加载、加载、卸载三种状态与内存/耗时指标。
    let temp = Temp::new("stats");
    let _dir = temp.pack("demo");
    write_pack(&_dir, MANIFEST, SCRIPT);
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
    let ran = runtime.invoke("demo.run", "");
    let loaded = runtime.stats();
    checks.push((
        "加载后有内存与派发计数",
        matches!(ran, Ok(Invoke::Done(_)))
            && loaded[0].state == ExtState::Loaded
            && loaded[0].dispatches >= 1
            && loaded[0].memory_used > 0,
        format!("run={ran:?} stat={:?}", loaded.first()),
    ));
    let peak = loaded[0].dispatch_peak_us;
    let budget_us = Limits::default().dispatch_ms * 1000;
    // CI 延迟门禁:一次普通命令的峰值必须落在步进预算内。
    checks.push((
        "CI 延迟门禁",
        peak <= budget_us,
        format!("派发峰值 {peak} us / 预算 {budget_us} us"),
    ));
    runtime.deactivate("demo");
    let idle = runtime.stats();
    checks.push((
        "空闲卸载后回到空闲",
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

    // 2. 死循环连续超时后停用;状态与超限次数都要能看见。
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST, SCRIPT);
    let mut pack = Pack::load(&dir).expect("加载死循环包");
    for _ in 0..lueying_extjs::MAX_FAULTS {
        let _ = invoke(&mut pack, "spin");
    }
    let stat = pack.stat();
    let stopped = invoke(&mut pack, "spin");
    checks.push((
        "连续超限停用",
        stat.state == ExtState::Disabled
            && stat.faults >= lueying_extjs::MAX_FAULTS
            && stat.timeouts >= u64::from(lueying_extjs::MAX_FAULTS)
            && stopped.contains("已停用"),
        format!(
            "state={:?} faults={} timeouts={} stopped={stopped}",
            stat.state, stat.faults, stat.timeouts
        ),
    ));

    // 3. 作业步超预算也计数并停用。
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST, JOB_SCRIPT);
    let mut pack = Pack::load(&dir).expect("加载作业包");
    let pending = pack.invoke("run", "{}");
    let settled = pack.pump(0);
    let stat = pack.stat();
    checks.push((
        "作业步超限计失败",
        matches!(pending, Ok(Invoke::Pending(_)))
            && settled.is_empty()
            && stat.timeouts >= 1
            && stat.faults >= 1,
        format!(
            "pending={pending:?} settled={} stat={stat:?}",
            settled.len()
        ),
    ));

    // 4. 快照往返、设置页摘要与 CLI 表格/JSON。
    let path = temp.root.join("ext-metrics.json");
    let loaded_stat = ExtStat {
        id: "demo".to_string(),
        name: "演示".to_string(),
        version: "1.0.0".to_string(),
        state: ExtState::Loaded,
        memory_used: 1 << 20,
        memory_limit: 8 << 20,
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
        version: lueying_extjs::METRICS_VERSION,
        updated_ms: 42,
        budgets: Budgets {
            memory_bytes: 8 << 20,
            stack_bytes: 256 << 10,
            init_ms: 100,
            dispatch_ms: 8,
            max_faults: 3,
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

    // 5. 参数解析:正式入口是 `lueying ext top`。
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
