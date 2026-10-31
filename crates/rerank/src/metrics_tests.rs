use std::time::Duration;

use super::*;

#[test]
fn empty_snapshot_is_zero() {
    let metrics = LatencyMetrics::default();
    assert_eq!(metrics.snapshot(), LatencySnapshot::default());
}

#[test]
fn buckets_land_in_expected_percentiles() {
    let metrics = LatencyMetrics::default();
    for _ in 0..90 {
        metrics.record(Duration::from_micros(800));
    }
    for _ in 0..10 {
        metrics.record(Duration::from_micros(10_000));
    }
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.count, 100);
    assert_eq!(snapshot.p50_us, 1_000);
    assert_eq!(snapshot.p95_us, 16_000);
    assert_eq!(snapshot.max_us, 10_000);
}

#[test]
fn slow_calls_have_bounded_bucket() {
    let metrics = LatencyMetrics::default();
    metrics.record(Duration::from_secs(2));
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.count, 1);
    assert_eq!(snapshot.max_us, 2_000_000);
    assert_eq!(snapshot.p99_us, 2_000_000);
}
