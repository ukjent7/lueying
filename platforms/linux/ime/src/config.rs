//! 配置与用户数据路径。
//!
//! Linux 遵循 XDG:`config.toml`、`apps.toml`、`punct.txt` 在
//! `$XDG_CONFIG_HOME/lueying`,`user.tsv` 与模型在 `$XDG_DATA_HOME/lueying`。
//! `LUEYING_CONFIG` 可整体改写配置位置,便于测试与便携安装。

use std::path::PathBuf;

use lueying_server::config::AppConfig;

/// 读取配置;文件缺失或坏值由服务端配置层按默认值处理。
pub fn load() -> AppConfig {
    match path() {
        Some(path) => lueying_server::config::load(&path),
        None => AppConfig::default(),
    }
}

pub fn path() -> Option<PathBuf> {
    lueying_server::config::path()
}

/// 用户词库路径;`$XDG_DATA_HOME/lueying/user.tsv`。
pub fn user_path() -> Option<PathBuf> {
    Some(data_dir()?.join("lueying").join("user.tsv"))
}

/// 用户数据根目录;`LUEYING_DATA_HOME` 给自动化测试隔离用。
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LUEYING_DATA_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

/// 模型目录;`LUEYING_MODEL_DIR` 优先,其次用户数据目录。
pub fn models_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LUEYING_MODEL_DIR") {
        return Some(PathBuf::from(dir));
    }
    Some(data_dir()?.join("lueying").join("models"))
}
