//! 用户配置:`%APPDATA%\lueying\config.toml`,设置窗与常驻服务共用。
//!
//! 顶层是用户层,`[ext.<id>]` 是扩展命名空间;按应用覆盖在 `apps.toml`。
//! schema 与合并逻辑在 `config_file`,未知键在读取时拒绝;坏值收集成
//! [`ConfigIssue`],设置页与日志按层展示。保存先写临时文件再改名。
//! 自定义标点(`punct.txt`)是独立数据文件,读取时并入配置。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lueying_engine::{Config, Fuzzy, Script};
use lueying_features::SchemeId;

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

/// 截图 OCR 引擎。系统 OCR 零依赖;PP-OCRv6 随包带模型,首次识别时加载。
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
    /// 扩展命令的全局热键绑定:(组合串, 完整命令 id);空表示没有绑定。
    pub bindings: Vec<(String, String)>,
    /// 动作候选触发词:(触发词, 完整命令 id);整串完全匹配才出现。
    pub triggers: Vec<(String, String)>,
    /// 命令面板总热键;空串表示关闭。需要两个以上修饰键,保留组合会被拒。
    pub panel_hotkey: String,
    /// 截图 OCR 引擎;模型缺失时 PP-OCR 会回明确错误。
    pub ocr_backend: OcrBackend,
    /// 对外 commit 通道;默认关。开启后本地工具可凭令牌借用上屏能力。
    pub commit_channel: bool,
    /// 停顿重排提供者:`bypass` 走旁路进程,缺省用内置 25M 模型。
    pub settled_rerank_provider: Option<String>,
    /// 旁路可执行文件路径;重排缝的提供者从这里拉起。
    pub bypass_exe: Option<String>,
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
            // 三个修饰键避开常见应用快捷键;不想要就在配置里写 off。
            panel_hotkey: "Ctrl+Alt+Shift+Space".to_string(),
            ocr_backend: OcrBackend::System,
            // 对外 commit 通道默认关;开启在设置里显式做,令牌随后生成。
            commit_channel: false,
            settled_rerank_provider: None,
            bypass_exe: None,
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

/// 配置文件路径(`%APPDATA%` 优先,退回 `%LOCALAPPDATA%`)。
pub fn path() -> Option<PathBuf> {
    let base =
        PathBuf::from(std::env::var_os("APPDATA").or_else(|| std::env::var_os("LOCALAPPDATA"))?);
    let dir = base.join("lueying");
    let path = dir.join("config.toml");
    if !path.exists() {
        migrate_legacy(&base, &dir, &path);
    }
    Some(path)
}

/// 一次性把旧 `config.ini`/`apps.ini` 转成 TOML;迁移失败就留空配置。
fn migrate_legacy(base: &Path, dir: &Path, path: &Path) {
    let config_candidates = [
        dir.join("config.ini"),
        base.join("pinyin-ime").join("config.ini"),
    ];
    let apps_candidates = [
        dir.join("apps.ini"),
        base.join("pinyin-ime").join("apps.ini"),
    ];
    let config_text = config_candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .and_then(|old| std::fs::read_to_string(old).ok());
    let apps_text = apps_candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .and_then(|old| std::fs::read_to_string(old).ok());
    if config_text.is_none() && apps_text.is_none() {
        return;
    }
    let mut cfg = AppConfig::default();
    if let Some(text) = config_text {
        crate::config_file::apply(
            &mut cfg,
            crate::config_file::from_ini(&text),
            "config.ini",
            &mut Vec::new(),
        );
    }
    if let Some(text) = apps_text {
        cfg.apps = crate::config_file::apps_from_ini(&text)
            .into_iter()
            .map(|(host, layer)| (host, layer.into()))
            .collect();
    }
    let _ = std::fs::create_dir_all(dir);
    let _ = save(path, &cfg);
    let _ = save_apps(&cfg.apps);
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

/// 扩展同意记录(`%APPDATA%\lueying\extensions\consent.json`)。
pub fn extensions_consent_path() -> Option<PathBuf> {
    Some(extensions_dir()?.join("consent.json"))
}

/// 扩展密钥目录(`%APPDATA%\lueying\secrets`);每个扩展一个 JSON 文件。
pub fn secrets_dir() -> Option<PathBuf> {
    user_file("secrets")
}

/// PP-OCRv6 模型目录:`LUEYING_PPOCR_DIR` 优先,其次用户目录
/// `%APPDATA%\lueying\models\ppocr`,最后程序目录的 `models/ppocr`。
/// 用户放的文件可以覆盖随包模型;三件齐全才算可用。
pub fn ppocr_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::var_os("LUEYING_PPOCR_DIR") {
        candidates.push(PathBuf::from(dir));
    }
    if let Some(dir) = ppocr_user_dir() {
        candidates.push(dir);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        candidates.push(parent.join("models").join("ppocr"));
    }
    candidates.into_iter().find(|dir| ppocr_complete(dir))
}

/// 用户放模型的目录;设置页的「目录」按钮打开这里。
pub fn ppocr_user_dir() -> Option<PathBuf> {
    user_file("models/ppocr")
}

/// 按需下载模型的用户目录;`LUEYING_MODEL_DIR` 给自动化测试隔离用。
pub fn models_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LUEYING_MODEL_DIR") {
        return Some(PathBuf::from(dir));
    }
    user_file("models")
}

/// 模型路径:用户目录优先,其次安装目录。重排模型不随基础安装分发时,
/// 下载完成后重启 Server 就能从用户目录加载。
pub fn model_path(name: &str) -> Option<PathBuf> {
    if let Some(dir) = models_dir() {
        let path = dir.join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    let mut path = std::env::current_exe().ok()?;
    path.set_file_name(name);
    path.is_file().then_some(path)
}

fn ppocr_complete(dir: &Path) -> bool {
    ["det.onnx", "rec.onnx", "dict.txt"]
        .iter()
        .all(|name| dir.join(name).is_file())
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

/// 能力调用审计(`%LOCALAPPDATA%\lueying\logs\ext-audit.log`)。
pub fn ext_audit_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    Some(base.join("lueying").join("logs").join("ext-audit.log"))
}

/// 对外 commit 通道的鉴权令牌(`%APPDATA%\lueying\commit-token`)。
/// 只在 `commit_channel` 开启后由常驻服务生成。
pub fn commit_token_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("APPDATA")?);
    Some(crate::commit::token_path(&base.join("lueying")))
}

/// 扩展运行指标快照(`%LOCALAPPDATA%\lueying\logs\ext-metrics.json`)。
pub fn ext_metrics_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    Some(base.join("lueying").join("logs").join("ext-metrics.json"))
}

/// 声明式皮肤(`%APPDATA%\lueying\skin.toml`);缺文件用默认主题。
pub fn skin_path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("APPDATA")?);
    Some(base.join("lueying").join("skin.toml"))
}

/// 读配置。文件缺失用默认值;TOML/值域错误收集进 `config_issues`,不阻断
/// 其余键。自定义标点从 `punct.txt` 并入,按应用层从 `apps.toml` 并入。
pub fn load(path: &Path) -> AppConfig {
    let mut cfg = AppConfig::default();
    let mut issues = Vec::new();
    // `.ini` 只给旧测试与迁移读;正式路径永远是 `config.toml`。
    let legacy = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("ini"));
    match std::fs::read_to_string(path) {
        Ok(text) if legacy => crate::config_file::apply(
            &mut cfg,
            crate::config_file::from_ini(&text),
            "config.ini",
            &mut issues,
        ),
        Ok(text) => match toml::from_str::<crate::config_file::FileConfig>(&text) {
            Ok(file) => crate::config_file::apply(&mut cfg, file, "config.toml", &mut issues),
            Err(error) => issues.push(ConfigIssue::new(
                "config.toml",
                format!("TOML 解析失败: {error}"),
            )),
        },
        Err(_) => {}
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

/// 把一份旧 `config.ini` 文本合并进现有配置;返回收集到的问题。
pub fn apply_legacy(text: &str, cfg: &mut AppConfig) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();
    crate::config_file::apply(
        cfg,
        crate::config_file::from_ini(text),
        "config.ini",
        &mut issues,
    );
    issues
}

/// 把一份旧 `apps.ini` 文本转成按应用层。
pub fn apps_from_legacy(text: &str) -> BTreeMap<String, AppOverride> {
    crate::config_file::apps_from_ini(text)
        .into_iter()
        .map(|(host, layer)| (host, layer.into()))
        .collect()
}

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

/// 保存按应用层(迁移或未来的设置页写入用)。
pub fn save_apps(apps: &BTreeMap<String, AppOverride>) -> std::io::Result<()> {
    let Some(path) = apps_path() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = crate::config_file::render_apps(apps)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// 运行期脚本扩展 id:与扩展清单同一套字符规则,同时保证能安全地当目录名。
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
