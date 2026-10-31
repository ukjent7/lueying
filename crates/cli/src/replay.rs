//! `lueying ext replay` 的无头回放:按键序列进,候选列表出。
//!
//! Linux 上没有 TSF 与候选窗,开发扩展与查回归都靠它:读一份按键脚本,
//! 驱动与服务端同一套引擎(内置迷你词库、默认全拼、功能模块注册表),
//! 触发词按服务端同一条路径注入动作候选,输出每步的上屏、组合与候选。
//! 回放是确定性的:全新用户词库,联想关闭,不读任何系统状态。

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use lueying_dict::Dictionary;
use lueying_engine::module::Modules;
use lueying_engine::{Action, Config, Engine, Event, Key, SharedUser, UserDict};
use lueying_extmeta::Manifest;
use lueying_features::{Prediction, SchemeId, Script, ScriptModule, registry_with_script, scheme};

/// `replay` 的参数。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplayArgs {
    /// 按键脚本;`None` 读标准输入。
    pub script: Option<PathBuf>,
    /// config.toml 路径;`None` 用默认位置,文件不存在就当空触发表。
    pub config: Option<PathBuf>,
    /// 命令行的触发词(`词=命令`);叠在配置之上,同名时命令行优先。
    pub triggers: Vec<(String, String)>,
}

/// 跑一遍回放,返回逐阶段的人类可读输出。
pub fn run(args: &ReplayArgs) -> Result<String, String> {
    replay_with(args, crate::extcmd::extensions_dir().as_deref())
}

/// [`run`] 的可注入版本;扩展目录由调用方给出,测试不用碰环境变量。
pub fn replay_with(args: &ReplayArgs, ext_root: Option<&Path>) -> Result<String, String> {
    let script = match &args.script {
        Some(path) => {
            fs::read_to_string(path).map_err(|e| format!("读 {}: {e}", path.display()))?
        }
        None => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| format!("读标准输入: {e}"))?;
            text
        }
    };
    let triggers = load_triggers(args)?;
    let titles = load_titles(ext_root);
    let user: SharedUser = Arc::new(RwLock::new(UserDict::new()));
    let mut engine = Engine::new(Arc::new(Dictionary::builtin()), Config::default())
        .with_user(user)
        .with_modules(modules())
        .with_scheme(scheme(SchemeId::Full, false));

    let mut out = String::new();
    for (index, line) in script.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let keys = parse_line(line).map_err(|e| format!("第 {} 行:{e}", index + 1))?;
        out.push_str(&format!("> {line}\n"));
        for key in keys {
            sync_action(&mut engine, &triggers, &titles);
            record(&mut out, &engine.process(key));
            // 最后一个键可能才凑满触发词;再同步一次让动作候选进入本行末的候选快照。
            sync_action(&mut engine, &triggers, &titles);
        }
        describe(&mut out, &engine);
    }
    Ok(out)
}

/// 功能模块注册表:与服务端同构,短语留空、联想关闭,保证输出确定。
fn modules() -> Arc<Modules> {
    registry_with_script(
        None,
        Arc::new(Prediction::with_enabled(false)),
        Arc::new(ScriptModule::new(Script::Simplified)),
    )
}

/// config.toml 的 `[triggers]` 加命令行旗标;同名词命令行覆盖配置。
fn load_triggers(args: &ReplayArgs) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    if let Some(path) = args.config.clone().or_else(default_config_path)
        && path.exists()
    {
        let text = fs::read_to_string(&path).map_err(|e| format!("读 {}: {e}", path.display()))?;
        let table: toml::Table = text
            .parse()
            .map_err(|e| format!("解析 {}: {e}", path.display()))?;
        if let Some(map) = table.get("triggers").and_then(|item| item.as_table()) {
            for (trigger, command) in map {
                let Some(command) = command.as_str() else {
                    continue;
                };
                out.push((
                    trigger.trim().to_ascii_lowercase(),
                    command.trim().to_string(),
                ));
            }
        }
    }
    for (trigger, command) in &args.triggers {
        out.retain(|(existing, _)| existing != trigger);
        out.push((trigger.clone(), command.clone()));
    }
    Ok(out)
}

/// 默认 config.toml:`LUEYING_CONFIG` 优先,退回 Windows 与 XDG 常规位置。
fn default_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LUEYING_CONFIG") {
        return Some(PathBuf::from(path));
    }
    if let Some(base) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(base).join("lueying").join("config.toml"));
    }
    let base = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(base)
            .join(".config")
            .join("lueying")
            .join("config.toml"),
    )
}

/// 已安装扩展的命令标题表:完整命令 id `<扩展 id>.<命令 id>` → 标题。
/// 与服务端同一条规则:标题不在表里的命令不出动作候选。
fn load_titles(ext_root: Option<&Path>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(root) = ext_root else {
        return out;
    };
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(bytes) = fs::read(entry.path().join("manifest.json")) else {
            continue;
        };
        let Ok(manifest) = Manifest::parse(&bytes) else {
            continue;
        };
        for command in manifest.commands() {
            out.push((
                format!("{}.{}", manifest.id, command.id),
                command.title.clone(),
            ));
        }
    }
    out
}

/// 与服务端 `sync_action` 同一条路径:整串输入(去隔音符)命中触发表,
/// 且命令标题在线,才注入动作候选。
fn sync_action(engine: &mut Engine, triggers: &[(String, String)], titles: &[(String, String)]) {
    let action = engine.composition().and_then(|comp| {
        let raw = comp.raw.replace('\'', "");
        let command = triggers
            .iter()
            .find(|(trigger, _)| *trigger == raw)
            .map(|(_, command)| command.clone())?;
        let title = titles
            .iter()
            .find(|(id, _)| *id == command)
            .map(|(_, title)| title.clone())?;
        Some(Action {
            trigger: raw,
            id: command,
            title,
        })
    });
    engine.set_action(action);
}

/// 上屏与动作即时回显;模式切换与直通键留给组合/候选行。
fn record(out: &mut String, events: &[Event]) {
    for event in events {
        match event {
            Event::Commit(text) => out.push_str(&format!("  上屏 {text}\n")),
            Event::Action(id) => out.push_str(&format!("  动作 {id}\n")),
            _ => {}
        }
    }
}

/// 每个阶段末的状态行:组合原文与候选列表。
fn describe(out: &mut String, engine: &Engine) {
    match engine.composition() {
        Some(comp) => {
            let candidates = engine.candidates();
            if candidates.is_empty() {
                out.push_str(&format!("  组合 {} · 无候选\n", comp.raw));
            } else {
                out.push_str(&format!(
                    "  组合 {} · 候选 {}\n",
                    comp.raw,
                    candidates.join(" / ")
                ));
            }
        }
        None => out.push_str("  组合 (无)\n"),
    }
}

/// 一行按键:字母数字逐键进;`<名字>` 是功能键;分号、引号与常用标点作标点键。
fn parse_line(line: &str) -> Result<Vec<Key>, String> {
    let mut keys = Vec::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let mut name = String::new();
                let closed = loop {
                    match chars.next() {
                        Some('>') => break true,
                        Some(inner) => name.push(inner),
                        None => break false,
                    }
                };
                if !closed {
                    return Err(format!("<{name} 缺闭合的 >"));
                }
                keys.extend(special(&name)?);
            }
            'a'..='z' | 'A'..='Z' => keys.push(Key::Letter(c.to_ascii_lowercase())),
            '0'..='9' => keys.push(Key::Digit(c as u8 - b'0')),
            ';' | '\'' | ',' | '.' | '/' | '`' => keys.push(Key::Punct(c)),
            other => {
                return Err(format!(
                    "看不懂的字符 {other:?};用字母数字、标点或 <功能键>"
                ));
            }
        }
    }
    Ok(keys)
}

/// `<功能键>` 名单;与服务端键位对齐,全小写。
fn special(name: &str) -> Result<Vec<Key>, String> {
    let key = match name.to_ascii_lowercase().as_str() {
        "space" => Key::Space,
        "enter" | "return" => Key::Enter,
        "esc" | "escape" => Key::Esc,
        "back" | "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "shift" => Key::Shift,
        other => {
            return Err(format!(
                "未知功能键 <{other}>;用 space/enter/esc/back/delete/方向键/翻页/shift"
            ));
        }
    };
    Ok(vec![key])
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
