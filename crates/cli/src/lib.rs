//! `lueying ext top` 的纯逻辑:参数解析、读指标快照、渲染表格。
//!
//! 真正的快照由常驻服务每秒写到 `%LOCALAPPDATA%\lueying\logs\ext-metrics.json`;
//! 这个工具只读文件,不连管道,也不会唤醒扩展。

use std::path::{Path, PathBuf};

use lueying_extmeta::metrics::ExtMetrics;

/// 帮助与错误提示。
pub const USAGE: &str = "\
用法: lueying ext top [选项]

查看扩展的内存、派发/步进峰值与超限次数。

选项:
  --file <路径>   指定指标快照;默认 %LOCALAPPDATA%\\lueying\\logs\\ext-metrics.json
  --json          输出快照 JSON
  --watch         每秒刷新
  -h, --help      显示帮助";

/// `top` 的选项。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopOptions {
    /// 指标快照路径;`None` 用默认路径。
    pub file: Option<PathBuf>,
    /// 输出 JSON。
    pub json: bool,
    /// 每秒刷新。
    pub watch: bool,
}

/// 一次命令行调用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    /// 查看指标。
    Top(TopOptions),
    /// 打印帮助。
    Help,
}

/// 解析 `lueying ext top ...` 的参数。
pub fn parse_args<I>(args: I) -> Result<Invocation, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("ext") => {}
        Some("-h" | "--help") | None => return Ok(Invocation::Help),
        Some(other) => return Err(format!("未知命令 {other:?};先写 `ext`")),
    }
    match args.next().as_deref() {
        Some("top") => {}
        Some("-h" | "--help") | None => return Ok(Invocation::Help),
        Some(other) => return Err(format!("未知子命令 {other:?};当前只有 `top`")),
    }
    let mut options = TopOptions::default();
    for arg in args.by_ref() {
        match arg.as_str() {
            "--json" => options.json = true,
            "--watch" => options.watch = true,
            "--file" => {
                let path = args.next().ok_or("--file 后面缺路径")?;
                options.file = Some(PathBuf::from(path));
            }
            "-h" | "--help" => return Ok(Invocation::Help),
            other => return Err(format!("未知参数 {other:?}")),
        }
    }
    Ok(Invocation::Top(options))
}

/// 默认快照路径:`LUEYING_EXT_METRICS` 优先,其次 Windows 的 LOCALAPPDATA,
/// 最后退回 XDG 状态目录(E2E/开发机)。
pub fn default_metrics_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LUEYING_EXT_METRICS") {
        return Some(PathBuf::from(path));
    }
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return Some(
            PathBuf::from(base)
                .join("lueying")
                .join("logs")
                .join("ext-metrics.json"),
        );
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(base.join("lueying").join("logs").join("ext-metrics.json"))
}

/// 读快照;文件不存在时给出可操作的错误。
pub fn read_metrics(path: &Path) -> Result<ExtMetrics, String> {
    if !path.exists() {
        return Err(format!(
            "没有运行指标: {} (常驻服务启动并启用扩展后才有)",
            path.display()
        ));
    }
    Ok(ExtMetrics::load(path))
}

/// 表格输出;首行是摘要,第二行是预算。
pub fn render_table(metrics: &ExtMetrics) -> String {
    let budgets = &metrics.budgets;
    let mut out = String::new();
    out.push_str(&metrics.summary());
    out.push('\n');
    out.push_str(&format!(
        "预算: 内存 {} MiB · 栈 {} KiB · 初始化 {} ms · 单步 {} ms · 连续 {} 次失败停用\n",
        budgets.memory_bytes >> 20,
        budgets.stack_bytes >> 10,
        budgets.init_ms,
        budgets.dispatch_ms,
        budgets.max_faults
    ));
    out.push_str(&format!("更新: {}\n\n", updated_ago(metrics.updated_ms)));
    out.push_str(&format!(
        "{:<18}{:<9}{:<10}{:<12}{:<12}{:<7}{:<7}{}\n",
        "扩展", "状态", "内存", "派发峰值", "步进峰值", "超时", "失败", "说明"
    ));
    for stat in &metrics.extensions {
        let note = stat
            .last_error
            .as_deref()
            .map(|note| note.replace(['\r', '\n'], " "))
            .unwrap_or_default();
        out.push_str(&format!(
            "{:<18}{:<9}{:<10}{:<12}{:<12}{:<7}{:<7}{}\n",
            stat.id,
            stat.state.title(),
            stat.memory_text(),
            stat.dispatch_peak_ms(),
            stat.step_peak_ms(),
            stat.timeouts,
            stat.faults,
            note
        ));
    }
    out
}

/// JSON 输出;与快照文件同格式。
pub fn render_json(metrics: &ExtMetrics) -> Result<String, String> {
    serde_json::to_string_pretty(metrics).map_err(|e| format!("序列化指标失败: {e}"))
}

fn updated_ago(updated_ms: u64) -> String {
    if updated_ms == 0 {
        return "未知".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    let seconds = now.saturating_sub(updated_ms) / 1000;
    match seconds {
        0 => "刚刚".to_string(),
        1..=59 => format!("{seconds} 秒前"),
        60..=3599 => format!("{} 分钟前", seconds / 60),
        _ => format!("{} 小时前", seconds / 3600),
    }
}
