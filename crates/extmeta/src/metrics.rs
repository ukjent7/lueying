//! 扩展运行指标快照:服务端写给 `lueying ext top` 与设置页。
//!
//! 快照落在 `%LOCALAPPDATA%\lueying\logs\ext-metrics.json`,只是观察数据,
//! 不是扩展的信任记录;读不到或读坏都按空快照处理,不影响扩展运行。

use std::path::Path;

use serde::{Deserialize, Serialize};

/// 快照格式版本。
pub const METRICS_VERSION: u32 = 1;
/// 指标文件的最大读取量;它只该几 KB。
const MAX_METRICS_BYTES: usize = 1024 * 1024;

/// 宿主给每个扩展的预算;展示用,不是新的配置项。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    /// JS 内存上限(字节)。
    pub memory_bytes: u64,
    /// JS 栈上限(字节)。
    pub stack_bytes: u64,
    /// 入口脚本初始化时限(毫秒)。
    pub init_ms: u64,
    /// 单次派发/作业步时限(毫秒)。
    pub dispatch_ms: u64,
    /// 连续失败多少次停用。
    pub max_faults: u32,
}

/// 一个扩展当前的运行状态。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtState {
    /// 命令包尚未加载,或已空闲卸载。
    #[default]
    Idle,
    /// JS 宿主已加载。
    Loaded,
    /// 连续失败或超限,已停用。
    Disabled,
    /// 清单/脚本/同意检查读失败。
    Error,
    /// 配置里启用了,但用户还没同意。
    Unapproved,
}

impl ExtState {
    /// 人话状态;CLI 与设置页共用。
    pub fn title(self) -> &'static str {
        match self {
            Self::Idle => "空闲",
            Self::Loaded => "运行",
            Self::Disabled => "已停用",
            Self::Error => "加载失败",
            Self::Unapproved => "待同意",
        }
    }
}

/// 一个扩展的指标。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtStat {
    /// 扩展 id。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 扩展版本。
    pub version: String,
    /// 当前状态。
    pub state: ExtState,
    /// QuickJS 当前占用(字节);未加载为 0。
    pub memory_used: u64,
    /// 内存上限(字节)。
    pub memory_limit: u64,
    /// 命令/场景派发次数。
    pub dispatches: u64,
    /// 作业步数。
    pub steps: u64,
    /// 单次派发最慢耗时(微秒)。
    pub dispatch_peak_us: u64,
    /// 单个作业步最慢耗时(微秒)。
    pub step_peak_us: u64,
    /// 派发/步进超时次数。
    pub timeouts: u64,
    /// 内存超限次数。
    pub ooms: u64,
    /// 连续失败计数。
    pub faults: u32,
    /// 尚未结算的 Promise 数。
    pub pending: u64,
    /// 停用或最近一次失败的原因。
    #[serde(default)]
    pub last_error: Option<String>,
}

impl ExtStat {
    /// 单次派发峰值换算成毫秒文本。
    pub fn dispatch_peak_ms(&self) -> String {
        millis(self.dispatch_peak_us)
    }

    /// 作业步峰值换算成毫秒文本。
    pub fn step_peak_ms(&self) -> String {
        millis(self.step_peak_us)
    }

    /// 当前占用换算成 MiB 文本。
    pub fn memory_text(&self) -> String {
        if self.memory_used == 0 {
            return "-".to_string();
        }
        format!("{:.1} MiB", self.memory_used as f64 / (1024.0 * 1024.0))
    }
}

/// 一份完整快照。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtMetrics {
    /// 格式版本。
    #[serde(default = "version")]
    pub version: u32,
    /// 写入时间,Unix 毫秒。
    pub updated_ms: u64,
    /// 当前预算。
    pub budgets: Budgets,
    /// 配置里启用的扩展;未同意与坏包也在列表里。
    #[serde(default)]
    pub extensions: Vec<ExtStat>,
}

impl ExtMetrics {
    /// 读快照;缺失、损坏或超大都按空快照处理。
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        if bytes.len() > MAX_METRICS_BYTES {
            return Self::default();
        }
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// 原子写回;先写临时文件再改名。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("建指标目录失败: {e}"))?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("序列化指标失败: {e}"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("写指标失败: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("替换指标失败: {e}"))
    }

    /// 所有扩展里最大的作业步耗时(微秒)。
    pub fn peak_step_us(&self) -> u64 {
        self.extensions
            .iter()
            .map(|stat| stat.step_peak_us)
            .max()
            .unwrap_or(0)
    }

    /// 超时与内存超限的总次数。
    pub fn violations(&self) -> u64 {
        self.extensions
            .iter()
            .map(|stat| stat.timeouts.saturating_add(stat.ooms))
            .fold(0, u64::saturating_add)
    }

    /// 设置页与 `top` 首行的一句话摘要。
    pub fn summary(&self) -> String {
        if self.extensions.is_empty() {
            return "没有启用的扩展".to_string();
        }
        let loaded = self.count(ExtState::Loaded);
        let disabled = self.count(ExtState::Disabled);
        let pending = self.count(ExtState::Unapproved);
        let mut text = format!("{} 个 · 运行 {loaded}", self.extensions.len());
        if disabled > 0 {
            text.push_str(&format!(" · 停用 {disabled}"));
        }
        if pending > 0 {
            text.push_str(&format!(" · 待同意 {pending}"));
        }
        text.push_str(&format!(" · 步进峰值 {}", millis(self.peak_step_us())));
        let violations = self.violations();
        if violations > 0 {
            text.push_str(&format!(" · 超限 {violations}"));
        }
        text
    }

    fn count(&self, state: ExtState) -> usize {
        self.extensions
            .iter()
            .filter(|stat| stat.state == state)
            .count()
    }
}

fn version() -> u32 {
    METRICS_VERSION
}

fn millis(micros: u64) -> String {
    format!("{:.1} ms", micros as f64 / 1000.0)
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
