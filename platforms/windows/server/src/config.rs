//! 用户配置:`%APPDATA%\lueying\config.toml`,设置窗与常驻服务共用。
//!
//! 顶层是用户层,`[ext.<id>]` 是扩展命名空间;按应用覆盖在 `apps.toml`。
//! schema 与合并逻辑在 `config_file`,未知键在读取时拒绝;坏值收集成
//! [`ConfigIssue`],设置页与日志按层展示。保存先写临时文件再改名。
//! 自定义标点(`punct.txt`)是独立数据文件,读取时并入配置。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lueying_core::schemes::SchemeId;
use lueying_core::{Config, Fuzzy, Script};

/// 一处配置问题:出错的文件层 + 人话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigIssue {
    pub layer: &'static str,
    pub message: String,
}

impl ConfigIssue {
    pub fn new(layer: &'static str, message: impl Into<String>) -> Self {
        Self {
            layer,
            message: message.into(),
        }
    }
}

/// 主题色名转下标;表在 `accents` 里,Windows 与否都能解析。
pub(crate) fn accent_by_name(value: &str) -> Option<usize> {
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

    /// 配置值转枚举;未知值返回 None,由调用方记一条 issue。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "general" => Some(Self::General),
            "tw" | "taiwan" => Some(Self::Taiwan),
            "hk" | "hongkong" => Some(Self::HongKong),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
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

    /// 配置值转枚举;未知值返回 None。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "horizontal" | "h" => Some(Self::Horizontal),
            "vertical" | "v" => Some(Self::Vertical),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
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

    /// 配置值转枚举;未知值返回 None。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "small" => Some(Self::Small),
            "standard" => Some(Self::Standard),
            "large" => Some(Self::Large),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
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

    /// 配置值转枚举;未知值返回 None。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Some(Self::Stable),
            "beta" | "test" => Some(Self::Beta),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }
}

/// 截图 OCR 引擎。系统 OCR 零依赖;PP-OCRv6 是原生扩展,首次识别拉起进程。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OcrBackend {
    #[default]
    System,
    Ppocr,
}

impl OcrBackend {
    pub const NAMES: [&'static str; 2] = ["系统", "PP-OCRv6"];

    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Ppocr,
            _ => Self::System,
        }
    }

    /// 配置值转枚举;未知值返回 None。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "system" => Some(Self::System),
            "ppocr" | "pp-ocr" | "paddle" => Some(Self::Ppocr),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Ppocr => "ppocr",
        }
    }
}

/// 单个应用的覆盖项;`None` 表示跟随全局设置。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppOverride {
    /// 中文标点开关。
    pub punct: Option<bool>,
    /// 学习开关(词库与词序都不写)。
    pub learning: Option<bool>,
}

/// 运行时配置。引擎只关心其中若干项,`shift_toggle` 由 Server 处理。
#[derive(Clone, Debug, PartialEq)]
pub struct AppConfig {
    pub page_size: usize,
    pub chinese_punct: bool,
    /// 停顿后 25M 重排是否允许换首选;关闭后首选只随按键变化。
    pub settled_promote: bool,
    /// 工具栏大小倍率,限定在 0.75–1.5。
    pub toolbar_scale: f32,
    /// 工具栏上次拖到的位置(屏幕坐标);没有就停在右下角。
    pub toolbar_pos: Option<(i32, i32)>,
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
    /// 是否启用候选窗弹性动效;关掉就直接呈现终态。
    pub effects: bool,
    /// 启用的运行期扩展 id,按组合顺序排列;空表示不启用。
    pub extensions: Vec<String>,
    /// 扩展命令的全局热键绑定:(组合串, 完整命令 id);空表示没有绑定。
    pub bindings: Vec<(String, String)>,
    /// 动作候选触发词:(触发词, 完整命令 id);整串完全匹配才出现。
    pub triggers: Vec<(String, String)>,
    /// 截图 OCR 引擎;模型缺失时 PP-OCR 会回明确错误。
    pub ocr_backend: OcrBackend,
    /// 更新渠道。
    pub update_channel: Channel,
    /// 自定义标点映射,来自 `punct.txt`。
    pub punct: BTreeMap<char, String>,
    /// 按应用覆盖层,来自 `apps.toml`;键是宿主 exe 名小写。
    pub apps: BTreeMap<String, AppOverride>,
    /// 扩展命名空间 `[ext.<id>]`,声明式设置与 `Request::Ext` 共用。
    pub ext_settings: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
    /// 读取时收集的问题;保存成功后清空。
    pub config_issues: Vec<ConfigIssue>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            page_size: 5,
            chinese_punct: true,
            settled_promote: true,
            toolbar_scale: 1.0,
            toolbar_pos: None,
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
            bindings: Vec::new(),
            triggers: Vec::new(),
            ocr_backend: OcrBackend::System,
            update_channel: Channel::Stable,
            punct: BTreeMap::new(),
            apps: BTreeMap::new(),
            ext_settings: BTreeMap::new(),
            config_issues: Vec::new(),
        }
    }
}

impl AppConfig {
    /// 目标输出字形;`traditional` 关闭时保持简体。
    pub fn script(&self) -> Script {
        if !self.traditional {
            return Script::Simplified;
        }
        match self.region {
            Region::General => Script::Traditional,
            Region::Taiwan => Script::Taiwan,
            Region::HongKong => Script::HongKong,
        }
    }

    /// 转成引擎配置。
    pub fn engine(&self) -> Config {
        Config {
            fuzzy: self.fuzzy,
            chinese_punct: self.chinese_punct,
            page_size: self.page_size,
            punct: self.punct.clone(),
        }
    }

    /// 取出宿主进程的覆盖项;没有就跟随全局。
    pub fn app_override(&self, host: &str) -> AppOverride {
        self.apps
            .get(&host.trim().to_ascii_lowercase())
            .copied()
            .unwrap_or_default()
    }
}

/// 配置文件路径(`%APPDATA%` 优先,退回 `%LOCALAPPDATA%`;Linux 走 XDG)。
pub fn path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LUEYING_CONFIG") {
        return Some(PathBuf::from(path));
    }
    #[cfg(windows)]
    {
        let base = PathBuf::from(
            std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?,
        );
        Some(base.join("lueying").join("config.toml"))
    }
    #[cfg(not(windows))]
    {
        Some(config_dir()?.join("lueying").join("config.toml"))
    }
}

/// 按应用覆盖文件路径(`%APPDATA%\lueying\apps.toml`)。
pub fn apps_path() -> Option<PathBuf> {
    user_file("apps.toml")
}

/// 自定义短语文件路径(`%APPDATA%\lueying\phrases.txt`)。
pub fn phrases_path() -> Option<PathBuf> {
    user_file("phrases.txt")
}

/// 自定义标点文件路径(`%APPDATA%\lueying\punct.txt`)。
pub fn punct_path() -> Option<PathBuf> {
    user_file("punct.txt")
}

/// 运行期扩展目录(`%APPDATA%\lueying\extensions`);每个包一个子目录。
pub fn extensions_dir() -> Option<PathBuf> {
    user_file("extensions")
}

/// 扩展宿主的两个本机目录:数据目录(`%APPDATA%\lueying`)与本地数据目录(`%LOCALAPPDATA%\lueying`)。
pub fn ext_host_dirs() -> Option<(PathBuf, PathBuf)> {
    let data = extensions_dir()?.parent()?.to_path_buf();
    let local = ext_metrics_path()?.parent()?.parent()?.to_path_buf();
    Some((data, local))
}

/// 扩展同意记录(`%APPDATA%\lueying\extensions\consent.json`)。
pub fn extensions_consent_path() -> Option<PathBuf> {
    Some(extensions_dir()?.join("consent.json"))
}

/// 扩展密钥目录(`%APPDATA%\lueying\secrets`);每个扩展一个 JSON 文件。
pub fn secrets_dir() -> Option<PathBuf> {
    user_file("secrets")
}

/// 扩展私有数据目录(`%APPDATA%\lueying\ext-data`);每个扩展一个 JSON 文件。
/// `LUEYING_EXT_DATA_DIR` 供 E2E 隔离。
pub fn ext_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LUEYING_EXT_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    user_file("ext-data")
}

/// PP-OCRv6 原生扩展目录:`LUEYING_PPOCR_DIR` 优先(测试隔离),其次
/// 扩展目录 `%APPDATA%\lueying\extensions\ppocr`。清单、可执行文件与
/// 三件模型(det.onnx、rec.onnx、dict.txt)都在一起,五件齐全才算可用。
pub fn ppocr_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LUEYING_PPOCR_DIR") {
        let path = PathBuf::from(dir);
        if ppocr_complete(&path) {
            return Some(path);
        }
    }
    let dir = extensions_dir()?.join("ppocr");
    ppocr_complete(&dir).then_some(dir)
}

fn ppocr_complete(dir: &Path) -> bool {
    [
        "manifest.json",
        "ppocr.exe",
        "det.onnx",
        "rec.onnx",
        "dict.txt",
    ]
    .iter()
    .all(|name| dir.join(name).is_file())
}

/// 按需下载模型的用户目录;`LUEYING_MODEL_DIR` 给自动化测试隔离用。
pub fn models_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LUEYING_MODEL_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(windows)]
    {
        user_file("models")
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })?;
        Some(base.join("lueying").join("models"))
    }
}

fn user_file(name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LUEYING_CONFIG") {
        return Path::new(&path).parent().map(|dir| dir.join(name));
    }
    #[cfg(windows)]
    {
        let base = PathBuf::from(
            std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?,
        );
        Some(base.join("lueying").join(name))
    }
    #[cfg(not(windows))]
    {
        Some(config_dir()?.join("lueying").join(name))
    }
}

#[cfg(not(windows))]
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
}

/// 服务日志路径(`%LOCALAPPDATA%\lueying\logs\server.log`)。
pub fn log_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    Some(base.join("lueying").join("logs").join("server.log"))
}

/// 扩展运行指标快照(`%LOCALAPPDATA%\lueying\logs\ext-metrics.json`)。
pub fn ext_metrics_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    Some(base.join("lueying").join("logs").join("ext-metrics.json"))
}

/// 剪贴板历史(`%APPDATA%\lueying\clipboard-history.json`)。
pub fn clipboard_history_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("APPDATA")?);
    Some(base.join("lueying").join("clipboard-history.json"))
}

/// 读配置。文件缺失用默认值;TOML/值域错误收集进 `config_issues`,不阻断
/// 其余键。自定义标点从 `punct.txt` 并入,按应用层从 `apps.toml` 并入。
pub fn load(path: &Path) -> AppConfig {
    let mut cfg = AppConfig::default();
    let mut issues = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        match toml::from_str::<crate::config_file::FileConfig>(&text) {
            Ok(file) => crate::config_file::apply(&mut cfg, file, "config.toml", &mut issues),
            Err(error) => issues.push(ConfigIssue::new(
                "config.toml",
                format!("TOML 解析失败: {error}"),
            )),
        }
    }
    cfg.apps = load_apps(&mut issues);
    cfg.punct = load_punct(punct_path().as_deref());
    for issue in &issues {
        eprintln!("配置问题({}): {}", issue.layer, issue.message);
    }
    cfg.config_issues = issues;
    cfg
}

/// 读按应用层;缺文件就是空层,坏文件记 issue 后返回空层。
fn load_apps(issues: &mut Vec<ConfigIssue>) -> BTreeMap<String, AppOverride> {
    let Some(path) = apps_path() else {
        return BTreeMap::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    match crate::config_file::apps_from_toml(&text) {
        Ok(layers) => layers
            .into_iter()
            .map(|(host, layer)| (host, layer.into()))
            .collect(),
        Err(error) => {
            issues.push(ConfigIssue::new("apps.toml", error));
            BTreeMap::new()
        }
    }
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

/// 按应用覆盖模板;实际文件是 `apps.toml`。
pub use crate::config_file::APPS_TEMPLATE;

/// 保存用户配置;目录不存在就创建。扩展命名空间原样写回。
pub fn save(path: &Path, cfg: &AppConfig) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = crate::config_file::render(cfg)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// 运行期扩展 id:与扩展清单同一套字符规则,同时保证能安全地当目录名。
pub(crate) fn valid_script_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && id.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
