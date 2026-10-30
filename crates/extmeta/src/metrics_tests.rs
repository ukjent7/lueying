use std::path::PathBuf;

use super::*;

fn stat(id: &str, state: ExtState, step_peak_us: u64) -> ExtStat {
    ExtStat {
        id: id.to_string(),
        name: format!("{id} 扩展"),
        version: "1.0.0".to_string(),
        state,
        memory_used: 1 << 20,
        memory_limit: 8 << 20,
        dispatches: 3,
        steps: 4,
        dispatch_peak_us: 700,
        step_peak_us,
        timeouts: 0,
        ooms: 0,
        faults: 0,
        pending: 1,
        last_error: None,
    }
}

fn temp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = std::env::temp_dir().join(format!(
        "lueying-metrics-{tag}-{}-{nanos}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    path
}

#[test]
fn roundtrip_keeps_stats_and_budgets() {
    let path = temp("roundtrip");
    let metrics = ExtMetrics {
        version: METRICS_VERSION,
        updated_ms: 42,
        budgets: Budgets {
            memory_bytes: 8 << 20,
            stack_bytes: 256 << 10,
            init_ms: 100,
            dispatch_ms: 8,
            max_faults: 3,
        },
        extensions: vec![
            stat("shot-ocr", ExtState::Loaded, 1500),
            stat("slow", ExtState::Disabled, 9000),
        ],
    };
    metrics.save(&path).expect("保存指标");
    assert_eq!(ExtMetrics::load(&path), metrics);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn corrupt_or_missing_is_empty() {
    let path = temp("corrupt");
    std::fs::write(&path, b"{not json").expect("写坏文件");
    assert_eq!(ExtMetrics::load(&path), ExtMetrics::default());
    let _ = std::fs::remove_file(&path);
    assert_eq!(ExtMetrics::load(&path), ExtMetrics::default());
}

#[test]
fn summary_counts_states_and_limits() {
    let metrics = ExtMetrics {
        version: METRICS_VERSION,
        updated_ms: 1,
        budgets: Budgets::default(),
        extensions: vec![
            stat("a", ExtState::Loaded, 1200),
            stat("b", ExtState::Disabled, 8000),
            stat("c", ExtState::Unapproved, 0),
        ],
    };
    let summary = metrics.summary();
    assert!(summary.contains("3 个"));
    assert!(summary.contains("运行 1"));
    assert!(summary.contains("停用 1"));
    assert!(summary.contains("待同意 1"));
    assert!(summary.contains("步进峰值 8.0 ms"));
    assert_eq!(metrics.peak_step_us(), 8000);

    let mut with_violations = metrics.clone();
    with_violations.extensions[0].timeouts = 2;
    with_violations.extensions[1].ooms = 1;
    assert!(with_violations.summary().contains("超限 3"));
    assert_eq!(with_violations.violations(), 3);
}

#[test]
fn stat_text_is_readable() {
    let loaded = stat("a", ExtState::Loaded, 1500);
    assert_eq!(loaded.step_peak_ms(), "1.5 ms");
    assert_eq!(loaded.dispatch_peak_ms(), "0.7 ms");
    assert_eq!(loaded.memory_text(), "1.0 MiB");
    let idle = ExtStat::default();
    assert_eq!(idle.memory_text(), "-");
    assert_eq!(ExtState::Unapproved.title(), "待同意");
}
