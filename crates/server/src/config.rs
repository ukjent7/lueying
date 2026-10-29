//! 用户配置:`%APPDATA%\lueying\config.ini`,设置窗与常驻服务共用。
//!
//! 行式 `键=值`,未知键和坏值跳过;保存先写临时文件再改名。
//! 自定义标点(`punct.txt`)、按应用覆盖(`apps.ini`)是独立文件,
//! 读取时并入配置,保存只写 `config.ini` 本身。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lueying_engine::{Config, Fuzzy, Script};
use lueying_features::SchemeId;

/// 主题色名转下标;表在 `accents` 里,Windows 与否都能解析。
fn accent_by_name(value: &str) -> Option<usize> {
    crate::accents::accent_index(value)
}

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

/// 候选窗排列。横排候选挤在一行里自动换行,竖排一行一条。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiLayout {
    #[default]
    Horizontal,
    Vertical,
}

impl UiLayout {
    pub const NAMES: [&'static str; 2] = ["横排", "竖排"];

    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Vertical,
            _ => Self::Horizontal,
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "vertical" | "v" => Self::Vertical,
            _ => Self::Horizontal,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

/// 候选字号档位;只缩放候选行,拼音行保持小字。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UiFont {
    Small,
    #[default]
    Standard,
    Large,
}

impl UiFont {
    pub const NAMES: [&'static str; 3] = ["小", "标准", "大"];

    /// 候选文字的缩放倍数;行高与估宽随之变化。
    pub fn factor(self) -> f32 {
        match self {
            Self::Small => 0.88,
            Self::Standard => 1.0,
            Self::Large => 1.15,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index {
            0 => Self::Small,
            2 => Self::Large,
            _ => Self::Standard,
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "small" => Self::Small,
            "large" => Self::Large,
            _ => Self::Standard,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Standard => "standard",
            Self::Large => "large",
        }
    }
}

/// 更新渠道。稳定只认正式 Release;测试跟进预发布。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    #[default]
    Stable,
    Beta,
}

impl Channel {
    pub const NAMES: [&'static str; 2] = ["稳定", "测试"];

    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Beta,
            _ => Self::Stable,
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "beta" | "test" => Self::Beta,
            _ => Self::Stable,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

/// 单个应用的覆盖项;`None` 表示跟随全局设置。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppOverride {
    /// 中文标点开关。
    pub punct: Option<bool>,
    /// 学习开关(词库、搭配与转移都不写)。
    pub learning: Option<bool>,
}

/// 运行时配置。引擎只关心其中若干项,`shift_toggle` 由 Server 处理。
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
    /// 候选窗排列:横排换行或竖排列。
    pub ui_layout: UiLayout,
    /// 候选字号档位。
    pub ui_font: UiFont,
    /// 主题色预设下标(`theme::ACCENTS`);高对比度主题忽略它。
    pub ui_accent: usize,
    /// 是否启用用户效果(SkSL 装饰层);关掉就只画素色。
    pub effects: bool,
    /// 启用的运行期脚本扩展 id,按组合顺序排列;空表示不启用。
    pub extensions: Vec<String>,
    /// 更新渠道。
    pub update_channel: Channel,
    /// 自定义标点映射,来自 `punct.txt`。
    pub punct: BTreeMap<char, String>,
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
            ui_layout: UiLayout::Horizontal,
            ui_font: UiFont::Standard,
            ui_accent: 0,
            effects: true,
            extensions: Vec::new(),
            update_channel: Channel::Stable,
            punct: BTreeMap::new(),
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
            punct: self.punct.clone(),
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
    user_file("phrases.txt")
}

/// 自定义标点文件路径(`%APPDATA%\lueying\punct.txt`)。
pub fn punct_path() -> Option<PathBuf> {
    user_file("punct.txt")
}

/// 按应用覆盖文件路径(`%APPDATA%\lueying\apps.ini`)。
pub fn apps_path() -> Option<PathBuf> {
    user_file("apps.ini")
}

/// 用户效果目录(`%APPDATA%\lueying\effects`);每个槽位一个 `.sksl` 文件。
pub fn effects_dir() -> Option<PathBuf> {
    let base =
        PathBuf::from(std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?);
    Some(base.join("lueying").join("effects"))
}

/// 运行期脚本扩展目录(`%APPDATA%\lueying\extensions`);每个包一个子目录。
pub fn extensions_dir() -> Option<PathBuf> {
    user_file("extensions")
}

fn user_file(name: &str) -> Option<PathBuf> {
    let base =
        PathBuf::from(std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?);
    Some(base.join("lueying").join(name))
}

/// 服务日志路径(`%LOCALAPPDATA%\lueying\logs\server.log`)。
pub fn log_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    Some(base.join("lueying").join("logs").join("server.log"))
}

/// 读配置;文件缺失或坏行都退回默认值。自定义标点从 `punct.txt` 并入。
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
            "ui_layout" | "layout" => {
                cfg.ui_layout = UiLayout::from_str(value);
            }
            "ui_font" | "font" => {
                cfg.ui_font = UiFont::from_str(value);
            }
            "ui_accent" | "accent" => {
                if let Ok(index) = value.parse::<usize>() {
                    cfg.ui_accent = index;
                } else if let Some(index) = accent_by_name(value) {
                    cfg.ui_accent = index;
                }
            }
            "update_channel" | "channel" => {
                cfg.update_channel = Channel::from_str(value);
            }
            // 用户效果开关:默认开(目录里没文件就是素色),关掉就整目录忽略。
            "effects" | "effect" => {
                if let Some(on) = flag(value) {
                    cfg.effects = on;
                }
            }
            // 运行期脚本扩展:逗号分隔的有序列表,顺序即组合顺序。
            "extensions" => cfg.extensions = parse_extensions(value),
            // 旧键:单个扩展 id;空值或 off 表示不启用。
            "script" | "extension" => {
                if matches!(value, "" | "off" | "none") {
                    cfg.extensions.clear();
                } else if valid_script_id(value) {
                    cfg.extensions = vec![value.to_string()];
                }
            }
            _ => {}
        }
    }
    cfg.punct = load_punct(punct_path().as_deref());
    cfg
}

/// 自定义标点文件模板;写进新文件,示例去掉行首 `#` 生效。
pub const PUNCT_TEMPLATE: &str = "\
# 掠影自定义标点:一行一条 `键=输出`。
# 键是单个半角符号,输出是任意文本(可为空,表示上屏空串)。
# 表里的符号固定按映射输出;表外的走默认中文标点。
# 示例(去掉行首 # 生效):
#,=、
#^=……
#\"=「
";

/// 读自定义标点;坏行与缺文件的键都跳过,映射值超过 16 个字符的行跳过。
pub fn load_punct(path: Option<&Path>) -> BTreeMap<char, String> {
    let mut map = BTreeMap::new();
    let Some(path) = path else {
        return map;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(key) = key.trim().chars().next() else {
            continue;
        };
        let value = value.trim();
        if value.chars().count() > 16 {
            continue;
        }
        map.insert(key, value.to_string());
    }
    map
}

/// 按应用覆盖文件模板;节名是宿主进程 exe 名(小写)。
pub const APPS_TEMPLATE: &str = "\
# 掠影按应用配置:节名是宿主进程的 exe 名(小写,如 notepad.exe)。
# 可用键:
#   punct=off     本应用关闭中文标点(直出半角)
#   punct=on      本应用开启中文标点
#   learning=off  本应用不写入学习记录
#   learning=on   本应用正常学习
# 没写的键跟随全局设置。示例(去掉行首 # 生效):
#[notepad.exe]
#punct=off
";

/// 解析按应用覆盖;`host` 需已小写。坏值与未知键跳过。
pub fn app_override(text: &str, host: &str) -> AppOverride {
    let mut current: Option<String> = None;
    let mut matched = AppOverride::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = Some(name.trim().to_ascii_lowercase());
            continue;
        }
        if current.as_deref() != Some(host) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let flag = flag(value.trim());
        match key.trim() {
            "punct" => matched.punct = flag,
            "learning" => matched.learning = flag,
            _ => {}
        }
    }
    matched
}

/// 读宿主进程的覆盖项;文件缺失时全部跟随全局。host 统一转小写。
pub fn app_overrides(host: &str) -> AppOverride {
    let Some(path) = apps_path() else {
        return AppOverride::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return AppOverride::default();
    };
    app_override(&text, &host.trim().to_ascii_lowercase())
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
         fuzzy={}\n\
         ui_layout={}\n\
         ui_font={}\n\
         ui_accent={}\n\
         effects={}\n\
         extensions={}\n\
         update_channel={}\n",
        cfg.page_size,
        cfg.chinese_punct,
        cfg.shift_toggle,
        cfg.traditional,
        cfg.region.as_str(),
        cfg.scheme.as_str(),
        cfg.aux,
        cfg.predict,
        if cfg.fuzzy.is_off() { "off" } else { "auto" },
        cfg.ui_layout.as_str(),
        cfg.ui_font.as_str(),
        cfg.ui_accent,
        if cfg.effects { "on" } else { "off" },
        cfg.extensions.join(","),
        cfg.update_channel.as_str(),
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

/// 运行期脚本扩展 id:与扩展清单同一套字符规则,同时保证能安全地当目录名。
fn valid_script_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && id.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// 逗号分隔的扩展列表:逐项校验、去重,坏项直接丢掉。
fn parse_extensions(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in value.split(',') {
        let id = item.trim();
        if valid_script_id(id) && !out.iter().any(|existing| existing == id) {
            out.push(id.to_string());
        }
    }
    out
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
