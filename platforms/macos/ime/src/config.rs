//! 用户配置:`~/Library/Application Support/lueying/config.toml`。
//!
//! 文件格式与 Windows 的 `config.toml` 相同,但 macOS 只读引擎层的键:
//! 页大小、中文标点与自定义标点。其余键(Windows 专用或扩展用的)直接忽略,
//! 不因此拒绝整个文件;坏值与解析错误回退到默认值,输入法照常可用。

use std::path::PathBuf;

use lueying_engine::Config;
use toml::{Table, Value};

/// 页大小的合法范围,与 Windows 设置页一致。
const PAGE_SIZE: std::ops::RangeInclusive<i64> = 3..=9;

/// 配置文件路径;没有 HOME 时为 `None`,只用默认值。
pub fn path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support/lueying")
            .join("config.toml"),
    )
}

/// 工具栏动画包的目录:与配置文件同一个目录下的 `toolbar-pack`。
pub fn toolbar_pack_dir() -> Option<PathBuf> {
    Some(path()?.parent()?.join("toolbar-pack"))
}

/// 用默认编辑器打开配置文件;没有就先建一个空的。设置页还没有 macOS 版,工具栏的「设」走这里。
pub fn open_for_editing() {
    let Some(file) = path() else {
        return;
    };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
    {
        eprintln!("打开配置文件 {} 失败: {e}", file.display());
        return;
    }
    let _ = std::process::Command::new("open").arg(&file).spawn();
}

/// 读取配置文件;缺失或读不出来都回退到默认值。
pub fn load() -> Config {
    let text = path()
        .and_then(|file| std::fs::read_to_string(file).ok())
        .unwrap_or_default();
    parse(&text)
}

/// 解析配置文本:只认引擎层的键,坏值保持默认。
fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    let Ok(table) = toml::from_str::<Table>(text) else {
        return cfg;
    };
    if let Some(size) = table.get("page_size").and_then(Value::as_integer)
        && PAGE_SIZE.contains(&size)
    {
        cfg.page_size = size as usize;
    }
    if let Some(on) = table.get("chinese_punct").and_then(Value::as_bool) {
        cfg.chinese_punct = on;
    }
    if let Some(punct) = table.get("punct").and_then(Value::as_table) {
        for (key, value) in punct {
            let mut chars = key.chars();
            if let (Some(c), None, Some(replacement)) = (chars.next(), chars.next(), value.as_str())
            {
                cfg.punct.insert(c, replacement.to_string());
            }
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn reads_engine_keys_and_ignores_the_rest() {
        let cfg = parse(
            "page_size = 7\nchinese_punct = false\nsettled_promote = true\n\n[punct]\n\",\" = \"、\"\n",
        );
        assert_eq!(cfg.page_size, 7);
        assert!(!cfg.chinese_punct);
        assert_eq!(cfg.punct.get(&','), Some(&"、".to_string()));
    }

    #[test]
    fn bad_values_fall_back_to_defaults() {
        assert_eq!(parse("page_size = 12").page_size, 5);
        assert!(parse("page_size = \"7\"").chinese_punct);
        assert_eq!(parse("not toml [").page_size, 5);
    }
}
