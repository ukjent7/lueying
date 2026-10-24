//! 用户配置:`%APPDATA%\pinyin-ime\config.ini`,设置窗与常驻服务共用。
//!
//! 行式 `键=值`,未知键和坏值跳过;保存先写临时文件再改名。

use std::path::{Path, PathBuf};

use pinyin_engine::{Config, Fuzzy};

/// 运行时配置。引擎只关心其中三项,`shift_toggle` 由 Server 处理。
#[derive(Clone, Debug, PartialEq)]
pub struct AppConfig {
    pub page_size: usize,
    pub chinese_punct: bool,
    pub fuzzy: Fuzzy,
    pub shift_toggle: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            page_size: 5,
            chinese_punct: true,
            fuzzy: Fuzzy::default(),
            shift_toggle: true,
        }
    }
}

impl AppConfig {
    /// 转成引擎配置。
    pub fn engine(&self) -> Config {
        Config {
            fuzzy: self.fuzzy,
            chinese_punct: self.chinese_punct,
            page_size: self.page_size,
        }
    }
}

/// 配置文件路径(`%APPDATA%` 优先,退回 `%LOCALAPPDATA%`)。
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?;
    Some(PathBuf::from(base).join("pinyin-ime").join("config.ini"))
}

/// 读配置;文件缺失或坏行都退回默认值。
pub fn load(path: &Path) -> AppConfig {
    let mut cfg = AppConfig::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return cfg;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "page_size" => {
                if let Ok(n) = value.parse::<usize>() {
                    cfg.page_size = n.clamp(3, 9);
                }
            }
            "chinese_punct" => {
                if let Some(on) = flag(value) {
                    cfg.chinese_punct = on;
                }
            }
            "shift_toggle" => {
                if let Some(on) = flag(value) {
                    cfg.shift_toggle = on;
                }
            }
            "fuzzy_zcs" => {
                if let Some(on) = flag(value) {
                    cfg.fuzzy.zcs = on;
                }
            }
            "fuzzy_ln" => {
                if let Some(on) = flag(value) {
                    cfg.fuzzy.ln = on;
                }
            }
            "fuzzy_fh" => {
                if let Some(on) = flag(value) {
                    cfg.fuzzy.fh = on;
                }
            }
            "fuzzy_an_ang" => {
                if let Some(on) = flag(value) {
                    cfg.fuzzy.an_ang = on;
                }
            }
            "fuzzy_en_eng" => {
                if let Some(on) = flag(value) {
                    cfg.fuzzy.en_eng = on;
                }
            }
            "fuzzy_in_ing" => {
                if let Some(on) = flag(value) {
                    cfg.fuzzy.in_ing = on;
                }
            }
            _ => {}
        }
    }
    cfg
}

/// 保存配置;目录不存在就创建。
pub fn save(path: &Path, cfg: &AppConfig) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = format!(
        "page_size={}\n\
         chinese_punct={}\n\
         shift_toggle={}\n\
         fuzzy_zcs={}\n\
         fuzzy_ln={}\n\
         fuzzy_fh={}\n\
         fuzzy_an_ang={}\n\
         fuzzy_en_eng={}\n\
         fuzzy_in_ing={}\n",
        cfg.page_size,
        cfg.chinese_punct,
        cfg.shift_toggle,
        cfg.fuzzy.zcs,
        cfg.fuzzy.ln,
        cfg.fuzzy.fh,
        cfg.fuzzy.an_ang,
        cfg.fuzzy.en_eng,
        cfg.fuzzy.in_ing,
    );
    let tmp = path.with_extension("ini.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn flag(value: &str) -> Option<bool> {
    match value {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}
