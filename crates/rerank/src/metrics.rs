//! 重排延迟的固定桶直方图:足够看 p50/p95,不引入浮点与依赖。
//!
//! 按键路径关心的是「大多数按键多快、尾巴多长」,不是精确平均值。
//! 桶边界按输入法可感知的毫秒级分档;两个阶段(4M 同步、25M 停顿)
//! 各持一份,会话共享。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 微秒桶上界;最后一个是兜底桶,避免超长调用被丢掉。
const BOUNDS_US: [u64; 13] = [
    100,
    250,
    500,
    1_000,
    2_000,
    4_000,
    8_000,
    16_000,
    32_000,
    64_000,
    128_000,
    256_000,
    u64::MAX,
];

/// 线程安全的延迟直方图。
#[derive(Debug, Default)]
pub struct LatencyMetrics {
    buckets: [AtomicU64; BOUNDS_US.len()],
    count: AtomicU64,
    sum_us: AtomicU64,
    max_us: AtomicU64,
}

/// 一次快照;百分位落在桶上界,值越大表示越慢。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatencySnapshot {
    pub count: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub mean_us: u64,
}

impl LatencyMetrics {
    /// 记一次调用耗时。`Duration` 是单调时钟差值,不受系统时间调整影响。
    pub fn record(&self, elapsed: Duration) {
        let micros = elapsed.as_micros().min(u128::from(u64::MAX)) as u64;
        let index = BOUNDS_US
            .iter()
            .position(|&bound| micros <= bound)
            .unwrap_or(BOUNDS_US.len() - 1);
        self.buckets[index].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.sum_us.fetch_add(micros, Ordering::Relaxed);
        self.max_us.fetch_max(micros, Ordering::Relaxed);
    }

    /// 当前分布;并发记录下各字段可能来自相邻时刻,统计用途可以接受。
    pub fn snapshot(&self) -> LatencySnapshot {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return LatencySnapshot::default();
        }
        let max_us = self.max_us.load(Ordering::Relaxed);
        let mean_us = self.sum_us.load(Ordering::Relaxed) / count;
        LatencySnapshot {
            count,
            p50_us: percentile(&self.buckets, count, 50, max_us),
            p95_us: percentile(&self.buckets, count, 95, max_us),
            p99_us: percentile(&self.buckets, count, 99, max_us),
            max_us,
            mean_us,
        }
    }
}

fn percentile(
    buckets: &[AtomicU64; BOUNDS_US.len()],
    count: u64,
    percent: u64,
    max_us: u64,
) -> u64 {
    let target = (count.saturating_mul(percent) + 99) / 100;
    let mut seen = 0;
    for (index, bucket) in buckets.iter().enumerate() {
        seen += bucket.load(Ordering::Relaxed);
        if seen >= target {
            return BOUNDS_US[index].min(max_us.max(1));
        }
    }
    max_us
}
