//! 用户配置:`%APPDATA%\lueying\config.ini`,设置窗与常驻服务共用。
//!
//! 行式 `键=值`,未知键和坏值跳过;保存先写临时文件再改名。

use std::path::{Path, PathBuf};

use lueying_engine::{Config, Fuzzy, Script};
use lueying_features::SchemeId;

/// 繁体字形地区;`traditional` 开着时生效。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Region {
    #[default]
    General,
    Taiwan,
    HongKong,
}

impl Region {
    pub const NAMES: [&'static str; 3] = ["通用", "台湾", "香港"];

    /// 设置页分段控件的选项下标转枚举;越界回通用。
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Taiwan,
            2 => Self::HongKong,
            _ => Self::General,
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "tw" | "taiwan" => Self::Taiwan,
            "hk" | "hongkong" => Self::HongKong,
            _ => Self::General,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Taiwan => "tw",
            Self::HongKong => "hk",
        }
    }
}

/// 运行时配置。引擎只关心其中五项,`shift_toggle` 由 Server 处理。
#[derive(Clone, Debug, PartialEq)]
pub struct AppConfig {
    pub page_size: usize,
    pub chinese_punct: bool,
    pub fuzzy: Fuzzy,
    pub shift_toggle: bool,
    pub traditional: bool,
    pub region: Region,
    /// 输入方案:全拼或双拼。
    pub scheme: SchemeId,
    /// 在拼音后按 `` ` `` 打 1–2 位五笔辅码。
    pub aux: bool,
    /// 按本机上屏词序补全下一词。
    pub predict: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            page_size: 5,
            chinese_punct: true,
            // 零配置:模糊音默认自动召回,设置页不再逐项开关。
            fuzzy: Fuzzy::auto(),
            shift_toggle: true,
            traditional: false,
            region: Region::General,
            scheme: SchemeId::Full,
            aux: false,
            predict: true,
        }
    }
}

impl AppConfig {
    /// 转成引擎配置。
    pub fn engine(&self) -> Config {
        let script = match self.region {
            Region::General => Script::Traditional,
            Region::Taiwan => Script::Taiwan,
            Region::HongKong => Script::HongKong,
        };
        Config {
            fuzzy: self.fuzzy,
            chinese_punct: self.chinese_punct,
            page_size: self.page_size,
            script: if self.traditional {
                script
            } else {
                Script::Simplified
            },
        }
    }
}

/// 配置文件路径(`%APPDATA%` 优先,退回 `%LOCALAPPDATA%`)。
pub fn path() -> Option<PathBuf> {
    let base =
        PathBuf::from(std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?);
    let dir = base.join("lueying");
    let path = dir.join("config.ini");
    // 一次性接走改名前的配置,升级后用户不必重设。
    if !path.exists() {
        let old = base.join("pinyin-ime").join("config.ini");
        if old.is_file() {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::copy(&old, &path);
        }
    }
    Some(path)
}

/// 自定义短语文件路径(`%APPDATA%\lueying\phrases.txt`)。
pub fn phrases_path() -> Option<PathBuf> {
    let base =
        PathBuf::from(std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?);
    Some(base.join("lueying").join("phrases.txt"))
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
            "traditional" => {
                if let Some(on) = flag(value) {
                    cfg.traditional = on;
                }
            }
            "traditional_region" => {
                cfg.region = Region::from_str(value);
            }
            "scheme" => {
                cfg.scheme = SchemeId::parse(value);
            }
            "aux_code" | "aux" => {
                if let Some(on) = flag(value) {
                    cfg.aux = on;
                }
            }
            "predict" | "prediction" => {
                if let Some(on) = flag(value) {
                    cfg.predict = on;
                }
            }
            // 隐藏逃生口:默认 auto;排障或极端用户可写 off。
            // 旧版 fuzzy_zcs/fuzzy_ln/... 逐项键不再读取,按默认自动召回。
            "fuzzy" => match value {
                "off" | "0" | "false" | "no" => cfg.fuzzy = Fuzzy::off(),
                "auto" | "1" | "true" | "on" | "yes" => cfg.fuzzy = Fuzzy::auto(),
                _ => {}
            },
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
         traditional={}\n\
         traditional_region={}\n\
         scheme={}\n\
         aux_code={}\n\
         predict={}\n\
         fuzzy={}\n",
        cfg.page_size,
        cfg.chinese_punct,
        cfg.shift_toggle,
        cfg.traditional,
        cfg.region.as_str(),
        cfg.scheme.as_str(),
        cfg.aux,
        cfg.predict,
        if cfg.fuzzy.is_off() { "off" } else { "auto" },
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
