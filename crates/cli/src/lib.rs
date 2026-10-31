//! `lueying ext` 命令行的纯逻辑:参数解析、`top` 的指标读取与表格渲染。
//!
//! 开发回路的文件与管道操作(new/pack/install/reload/logs)在 [`extcmd`];
//! 这里只放参数解析与 `top` 的只读展示,方便单测与 E2E 直接调用。

use std::path::{Path, PathBuf};

use lueying_extmeta::metrics::ExtMetrics;

pub mod extcmd;
pub mod replay;

pub use extcmd::{InstallArgs, LogsArgs, NewArgs, PackArgs};
pub use replay::ReplayArgs;

/// 帮助与错误提示。
pub const USAGE: &str = "\
用法: lueying ext <子命令> [选项]

扩展的开发回路:新建骨架、打包、安装、重载,以及日志、运行指标与对外上屏。

子命令:
  new <id>          新建扩展骨架(manifest.json + index.js)
  pack <目录>       打包成 .luext 文本包
  install <源>      安装目录或 .luext 到扩展目录
  reload            让常驻服务重读配置与扩展
  logs              看服务日志末尾
  top               查看扩展运行指标(内存、峰值、超限)
  replay [脚本]     无头回放:按键序列进,候选列表出(缺省读标准输入)
  commit <文本>     借用输入法把文本上屏到当前输入框(需开启 commit_channel)
  run <命令 id>     触发扩展命令;PowerToys 命令面板等启动器走这里
  commands          列出可触发的扩展命令(标题表)

选项:
  -h, --help        显示帮助

各子命令参数:
  new     --name <名称> --dir <目录>
  pack    --out <路径>
  install --dir <目录> --force
  logs    --file <路径> --tail <行数>
  top     --file <路径> --json --watch
  replay  --config <路径> --trigger <词=命令>";

/// 一次命令行调用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    /// 新建骨架。
    New(NewArgs),
    /// 打包目录。
    Pack(PackArgs),
    /// 安装包或目录。
    Install(InstallArgs),
    /// 重读配置。
    Reload,
    /// 看日志末尾。
    Logs(LogsArgs),
    /// 查看指标。
    Top(TopOptions),
    /// 无头回放。
    Replay(ReplayArgs),
    /// 借用上屏能力写文本;多个词已按空格合并。
    Commit(String),
    /// 触发扩展命令。
    Run(String),
    /// 列出可触发的扩展命令。
    Commands,
    /// 打印帮助。
    Help,
}

/// 解析 `lueying ext <子命令> ...` 的参数。
pub fn parse_args<I>(args: I) -> Result<Invocation, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some("ext") => {}
        Some("-h" | "--help") | None => return Ok(Invocation::Help),
        Some(other) => return Err(format!("未知命令 {other:?};从 `lueying ext` 开始")),
    }
    let sub = args
        .next()
        .ok_or("缺子命令;用 new/pack/install/reload/logs/top/replay/commit/run/commands")?;
    match sub.as_str() {
        "new" => parse_new(args),
        "pack" => parse_pack(args),
        "install" => parse_install(args),
        "reload" => parse_reload(args),
        "logs" => parse_logs(args),
        "top" => parse_top(args),
        "replay" => parse_replay(args),
        "commit" => parse_commit(args),
        "run" => parse_run(args),
        "commands" => parse_commands(args),
        "-h" | "--help" => Ok(Invocation::Help),
        other => Err(format!(
            "未知子命令 {other:?};用 new/pack/install/reload/logs/top/replay/commit/run/commands"
        )),
    }
}

/// 位置参数加旗标的通用走法:第一个不以 `-` 开头的是位置参数,出现第二个报错。
fn take_positional(
    used: &mut bool,
    slot: &mut Option<PathBuf>,
    arg: &str,
    what: &str,
) -> Result<(), String> {
    if *used {
        return Err(format!("多余的参数 {arg:?};{what} 只接受一个"));
    }
    *slot = Some(PathBuf::from(arg));
    *used = true;
    Ok(())
}

fn parse_new<I>(mut args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    let mut id: Option<PathBuf> = None;
    let mut id_used = false;
    let mut name: Option<String> = None;
    let mut dir: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--name" => name = Some(args.next().ok_or("--name 后面缺名称")?),
            "--dir" => dir = Some(PathBuf::from(args.next().ok_or("--dir 后面缺目录")?)),
            "-h" | "--help" => return Ok(Invocation::Help),
            other if other.starts_with('-') => return Err(format!("未知参数 {other:?}")),
            other => take_positional(&mut id_used, &mut id, other, "id")?,
        }
    }
    let id = id
        .ok_or("用法: lueying ext new <id> [--name 名称] [--dir 目录]")?
        .to_string_lossy()
        .into_owned();
    Ok(Invocation::New(NewArgs { id, name, dir }))
}

fn parse_pack<I>(mut args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    let mut dir: Option<PathBuf> = None;
    let mut dir_used = false;
    let mut out: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out 后面缺路径")?)),
            "-h" | "--help" => return Ok(Invocation::Help),
            other if other.starts_with('-') => return Err(format!("未知参数 {other:?}")),
            other => take_positional(&mut dir_used, &mut dir, other, "目录")?,
        }
    }
    let dir = dir.ok_or("用法: lueying ext pack <目录> [--out 路径]")?;
    Ok(Invocation::Pack(PackArgs { dir, out }))
}

fn parse_install<I>(mut args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    let mut source: Option<PathBuf> = None;
    let mut source_used = false;
    let mut dir: Option<PathBuf> = None;
    let mut force = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(args.next().ok_or("--dir 后面缺目录")?)),
            "--force" => force = true,
            "-h" | "--help" => return Ok(Invocation::Help),
            other if other.starts_with('-') => return Err(format!("未知参数 {other:?}")),
            other => take_positional(&mut source_used, &mut source, other, "源")?,
        }
    }
    let source =
        source.ok_or("用法: lueying ext install <目录或 .luext> [--dir 目录] [--force]")?;
    Ok(Invocation::Install(InstallArgs { source, dir, force }))
}

/// 文本子命令:其余参数按空格合并成待上屏文本。本命令没有旗标,
/// 以 `-` 开头的词也原样收进文本。
fn parse_commit<I>(args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    let words: Vec<String> = args.collect();
    if words.is_empty() {
        return Err("用法: lueying ext commit <文本>".into());
    }
    Ok(Invocation::Commit(words.join(" ")))
}

/// 命令 id 子命令:恰好一个位置参数。清单 id 无空格,多给就报错。
fn parse_run<I>(args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    let mut id: Option<String> = None;
    for arg in args {
        if id.is_some() {
            return Err(format!("多余的参数 {arg:?};命令 id 是一个,如 shot-ocr.run"));
        }
        if matches!(arg.as_str(), "-h" | "--help") {
            return Ok(Invocation::Help);
        }
        id = Some(arg);
    }
    let id = id.ok_or("用法: lueying ext run <命令 id>")?;
    Ok(Invocation::Run(id))
}

fn parse_commands<I>(mut args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    if let Some(arg) = args.next() {
        return match arg.as_str() {
            "-h" | "--help" => Ok(Invocation::Help),
            other => Err(format!("commands 不接受参数 {other:?}")),
        };
    }
    Ok(Invocation::Commands)
}

fn parse_reload<I>(mut args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    if let Some(arg) = args.next() {
        return match arg.as_str() {
            "-h" | "--help" => Ok(Invocation::Help),
            other => Err(format!("reload 不接受参数 {other:?}")),
        };
    }
    Ok(Invocation::Reload)
}

fn parse_logs<I>(mut args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    let mut file: Option<PathBuf> = None;
    let mut tail: Option<usize> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--file" => file = Some(PathBuf::from(args.next().ok_or("--file 后面缺路径")?)),
            "--tail" => {
                let value = args.next().ok_or("--tail 后面缺行数")?;
                tail = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--tail 不是行数: {value:?}"))?,
                );
            }
            "-h" | "--help" => return Ok(Invocation::Help),
            other => return Err(format!("未知参数 {other:?}")),
        }
    }
    Ok(Invocation::Logs(LogsArgs {
        file,
        tail: tail.unwrap_or(80),
    }))
}

fn parse_top<I>(mut args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    let mut options = TopOptions::default();
    while let Some(arg) = args.next() {
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

fn parse_replay<I>(mut args: I) -> Result<Invocation, String>
where
    I: Iterator<Item = String>,
{
    let mut script: Option<PathBuf> = None;
    let mut script_used = false;
    let mut config: Option<PathBuf> = None;
    let mut triggers = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                config = Some(PathBuf::from(args.next().ok_or("--config 后面缺路径")?));
            }
            "--trigger" => {
                let pair = args.next().ok_or("--trigger 后面缺 词=命令")?;
                let (trigger, command) = pair
                    .split_once('=')
                    .ok_or("--trigger 要 词=命令 形式,如 --trigger ocr=demo.run")?;
                let trigger = trigger.trim().to_ascii_lowercase();
                let command = command.trim().to_string();
                if trigger.is_empty() || command.is_empty() {
                    return Err("--trigger 的词与命令都不能为空".into());
                }
                triggers.push((trigger, command));
            }
            "-h" | "--help" => return Ok(Invocation::Help),
            other if other.starts_with('-') => return Err(format!("未知参数 {other:?}")),
            other => take_positional(&mut script_used, &mut script, other, "脚本文件")?,
        }
    }
    Ok(Invocation::Replay(ReplayArgs {
        script,
        config,
        triggers,
    }))
}

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
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
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

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
