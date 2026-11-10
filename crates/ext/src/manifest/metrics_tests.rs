use std::path::PathBuf;

use super::*;

fn stat(id: &str, state: ExtState, dispatch_peak_us: u64) -> ExtStat {
    ExtStat {
        id: id.to_string(),
        name: format!("{id} 扩展"),
        version: "1.0.0".to_string(),
        state,
        dispatches: 3,
        dispatch_peak_us,
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
fn roundtrip_keeps_stats() {
    let path = temp("roundtrip");
    let metrics = ExtMetrics {
        version: METRICS_VERSION,
        updated_ms: 42,
        extensions: vec![
            stat("shot-ocr", ExtState::Loaded, 1500),
            stat("pending", ExtState::Unapproved, 0),
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
fn summary_counts_states_and_peak() {
    let metrics = ExtMetrics {
        version: METRICS_VERSION,
        updated_ms: 1,
        extensions: vec![
            stat("a", ExtState::Loaded, 1200),
            stat("b", ExtState::Error, 8000),
            stat("c", ExtState::Unapproved, 0),
        ],
    };
    let summary = metrics.summary();
    assert!(summary.contains("3 个"));
    assert!(summary.contains("运行 1"));
    assert!(summary.contains("待同意 1"));
    assert!(summary.contains("派发峰值 8.0 ms"));
    assert_eq!(metrics.peak_dispatch_us(), 8000);
}

#[test]
fn stat_text_is_readable() {
    let loaded = stat("a", ExtState::Loaded, 700);
    assert_eq!(loaded.dispatch_peak_ms(), "0.7 ms");
    assert_eq!(ExtState::Unapproved.title(), "待同意");
}
