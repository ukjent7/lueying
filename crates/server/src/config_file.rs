//! `config.toml` 的 schema、合并与渲染。
//!
//! 分层顺序是默认值(代码)→ 用户 `config.toml` → 按应用 `apps.toml`
//! → 扩展命名空间 `[ext.<id>]`。未知键由 serde 直接拒绝;值域错误
//! 收集成 [`ConfigIssue`],设置页与日志按层展示,坏值不阻断其余配置。

use std::collections::BTreeMap;

use lueying_features::SchemeId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{
    AppConfig, AppOverride, Channel, ConfigIssue, OcrBackend, Region, UiFont, UiLayout,
    accent_by_name, valid_script_id,
};

/// 用户配置文件 schema。字段都是可选层,合并时只覆盖写了的项。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FileConfig {
    pub page_size: Option<usize>,
    pub chinese_punct: Option<bool>,
    pub fuzzy: Option<FuzzyValue>,
    pub shift_toggle: Option<bool>,
    pub traditional: Option<bool>,
    pub traditional_region: Option<String>,
    pub scheme: Option<String>,
    pub aux_code: Option<bool>,
    pub predict: Option<bool>,
    pub ui_layout: Option<String>,
    pub ui_font: Option<String>,
    pub ui_accent: Option<AccentValue>,
    pub effects: Option<bool>,
    pub extensions: Option<Vec<String>>,
    pub bindings: Option<BTreeMap<String, String>>,
    pub triggers: Option<BTreeMap<String, String>>,
    pub panel_hotkey: Option<String>,
    pub ocr_engine: Option<String>,
    pub update_channel: Option<String>,
    /// 扩展命名空间:`[ext.<id>]` 下的自由键值。
    pub ext: Option<BTreeMap<String, BTreeMap<String, Value>>>,
}

/// `fuzzy` 接受 `true`/`false` 或 `"auto"`/`"off"`。
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum FuzzyValue {
    Bool(bool),
    Mode(String),
}

/// `ui_accent` 接受下标或颜色名。
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum AccentValue {
    Index(usize),
    Name(String),
}

/// 按应用层:apps.toml 里每个 exe 一节。
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct AppLayer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub punct: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub learning: Option<bool>,
}

impl From<AppLayer> for AppOverride {
    fn from(layer: AppLayer) -> Self {
        Self {
            punct: layer.punct,
            learning: layer.learning,
        }
    }
}

/// 把一层配置合并进运行时配置。坏值记一条 issue,继续读其余键。
pub fn apply(
    cfg: &mut AppConfig,
    file: FileConfig,
    layer: &'static str,
    issues: &mut Vec<ConfigIssue>,
) {
    let mut note = |message: String| issues.push(ConfigIssue::new(layer, message));

    if let Some(size) = file.page_size {
        if (3..=9).contains(&size) {
            cfg.page_size = size;
        } else {
            note(format!("page_size={size} 超出 3–9,已忽略"));
        }
    }
    if let Some(on) = file.chinese_punct {
        cfg.chinese_punct = on;
    }
    if let Some(fuzzy) = file.fuzzy {
        match fuzzy {
            FuzzyValue::Bool(true) => cfg.fuzzy = lueying_engine::Fuzzy::auto(),
            FuzzyValue::Bool(false) => cfg.fuzzy = lueying_engine::Fuzzy::off(),
            FuzzyValue::Mode(mode) => match mode.as_str() {
                "auto" | "on" => cfg.fuzzy = lueying_engine::Fuzzy::auto(),
                "off" => cfg.fuzzy = lueying_engine::Fuzzy::off(),
                _ => note(format!("fuzzy={mode} 不合法,只接受 auto/off")),
            },
        }
    }
    if let Some(on) = file.shift_toggle {
        cfg.shift_toggle = on;
    }
    if let Some(on) = file.traditional {
        cfg.traditional = on;
    }
    if let Some(value) = file.traditional_region {
        match Region::parse(&value) {
            Some(region) => cfg.region = region,
            None => note(format!("traditional_region={value} 不合法")),
        }
    }
    if let Some(value) = file.scheme {
        match scheme(&value) {
            Some(scheme) => cfg.scheme = scheme,
            None => note(format!("scheme={value} 不合法")),
        }
    }
    if let Some(on) = file.aux_code {
        cfg.aux = on;
    }
    if let Some(on) = file.predict {
        cfg.predict = on;
    }
    if let Some(value) = file.ui_layout {
        match UiLayout::parse(&value) {
            Some(layout) => cfg.ui_layout = layout,
            None => note(format!("ui_layout={value} 不合法")),
        }
    }
    if let Some(value) = file.ui_font {
        match UiFont::parse(&value) {
            Some(font) => cfg.ui_font = font,
            None => note(format!("ui_font={value} 不合法")),
        }
    }
    if let Some(value) = file.ui_accent {
        match value {
            AccentValue::Index(index) => cfg.ui_accent = index,
            AccentValue::Name(name) => match accent_by_name(&name) {
                Some(index) => cfg.ui_accent = index,
                None => note(format!("ui_accent={name} 不是已知颜色")),
            },
        }
    }
    if let Some(on) = file.effects {
        cfg.effects = on;
    }
    if let Some(list) = file.extensions {
        let mut out = Vec::new();
        for item in list {
            let id = item.trim();
            if valid_script_id(id) {
                if !out.iter().any(|existing| existing == id) {
                    out.push(id.to_string());
                }
            } else {
                note(format!("extensions 里的 {id:?} 不是合法扩展 id"));
            }
        }
        cfg.extensions = out;
    }
    if let Some(map) = file.bindings {
        let mut out = Vec::new();
        for (combo, command) in map {
            let (combo, command) = (combo.trim(), command.trim());
            if combo.is_empty() || command.is_empty() {
                note("bindings 里有空组合或空命令,已忽略".to_string());
                continue;
            }
            if out.iter().any(|(existing, _)| existing == combo) {
                note(format!("bindings 里 {combo} 重复,保留第一条"));
                continue;
            }
            out.push((combo.to_string(), command.to_string()));
        }
        cfg.bindings = out;
    }
    if let Some(map) = file.triggers {
        let mut out: Vec<(String, String)> = Vec::new();
        for (trigger, command) in map {
            let trigger = trigger.trim().to_ascii_lowercase();
            let command = command.trim();
            if trigger.is_empty()
                || command.is_empty()
                || !trigger.bytes().all(|b| b.is_ascii_lowercase())
            {
                note(format!("triggers 里的 {trigger:?} 不合法,已忽略"));
                continue;
            }
            if out.iter().any(|(existing, _)| existing == &trigger) {
                note(format!("triggers 里 {trigger} 重复,保留第一条"));
                continue;
            }
            out.push((trigger, command.to_string()));
        }
        cfg.triggers = out;
    }
    if let Some(hotkey) = file.panel_hotkey {
        cfg.panel_hotkey = match hotkey.trim() {
            "" | "off" | "none" => String::new(),
            combo => combo.to_string(),
        };
    }
    if let Some(value) = file.ocr_engine {
        match OcrBackend::parse(&value) {
            Some(backend) => cfg.ocr_backend = backend,
            None => note(format!("ocr_engine={value} 不合法")),
        }
    }
    if let Some(value) = file.update_channel {
        match Channel::parse(&value) {
            Some(channel) => cfg.update_channel = channel,
            None => note(format!("update_channel={value} 不合法")),
        }
    }
    if let Some(ext) = file.ext {
        for (id, table) in ext {
            if !valid_script_id(&id) {
                note(format!("[ext.{id}] 不是合法扩展 id"));
                continue;
            }
            if table.len() > 64 {
                note(format!("[ext.{id}] 超过 64 个键,已忽略"));
                continue;
            }
            let mut target = BTreeMap::new();
            for (key, value) in table {
                if !valid_setting_key(&key) {
                    note(format!("[ext] 里的键 {key:?} 不合法,已忽略"));
                    continue;
                }
                if value.is_null() {
                    note(format!("[ext] 的 {key} 不能是 null"));
                    continue;
                }
                target.insert(key, value);
            }
            if !target.is_empty() {
                cfg.ext_settings.insert(id, target);
            }
        }
    }
}

/// 方案名;与 `SchemeId::parse` 的别名保持一致,但未知值返回 None。
fn scheme(value: &str) -> Option<SchemeId> {
    match value.trim().to_ascii_lowercase().as_str() {
        "full" | "quanpin" => Some(SchemeId::Full),
        "xiaohe" | "flypy" => Some(SchemeId::Xiaohe),
        "ziranma" | "zrm" => Some(SchemeId::Ziranma),
        "mspy" | "microsoft" => Some(SchemeId::Mspy),
        "sogou" => Some(SchemeId::Sogou),
        _ => None,
    }
}

/// 扩展命名空间里的键:与清单的 key 规则一致,稳定且能安全写进 TOML。
pub fn valid_setting_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && key.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// 读取旧版 `config.ini`:只用于测试与一次性迁移,未知键忽略。
pub fn from_ini(text: &str) -> FileConfig {
    let mut file = FileConfig::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "page_size" => file.page_size = value.parse().ok(),
            "chinese_punct" => file.chinese_punct = bool_value(value),
            "shift_toggle" => file.shift_toggle = bool_value(value),
            "traditional" => file.traditional = bool_value(value),
            "traditional_region" => file.traditional_region = Some(value.to_string()),
            "scheme" => file.scheme = Some(value.to_string()),
            "aux_code" | "aux" => file.aux_code = bool_value(value),
            "predict" | "prediction" => file.predict = bool_value(value),
            "fuzzy" => {
                file.fuzzy = Some(match value {
                    "off" | "0" | "false" | "no" => FuzzyValue::Bool(false),
                    "auto" | "1" | "true" | "on" | "yes" => FuzzyValue::Bool(true),
                    other => FuzzyValue::Mode(other.to_string()),
                })
            }
            "ui_layout" | "layout" => file.ui_layout = Some(value.to_string()),
            "ui_font" | "font" => file.ui_font = Some(value.to_string()),
            "ui_accent" | "accent" => {
                file.ui_accent = Some(match value.parse::<usize>() {
                    Ok(index) => AccentValue::Index(index),
                    Err(_) => AccentValue::Name(value.to_string()),
                })
            }
            "update_channel" | "channel" => file.update_channel = Some(value.to_string()),
            "effects" | "effect" => file.effects = bool_value(value),
            "extensions" => file.extensions = Some(parse_list(value)),
            "bindings" | "bind" => file.bindings = Some(pairs(value)),
            "triggers" | "trigger" => file.triggers = Some(pairs(value)),
            "panel_hotkey" | "palette_hotkey" => file.panel_hotkey = Some(value.to_string()),
            "ocr_engine" | "ocr" => file.ocr_engine = Some(value.to_string()),
            "script" | "extension" => {
                file.extensions = Some(if matches!(value, "" | "off" | "none") {
                    Vec::new()
                } else {
                    vec![value.to_string()]
                });
            }
            _ => {}
        }
    }
    file
}

/// 读取旧版 `apps.ini`:节名是 exe 名,键是 punct/learning。
pub fn apps_from_ini(text: &str) -> BTreeMap<String, AppLayer> {
    let mut out = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = Some(name.trim().to_ascii_lowercase());
            continue;
        }
        let Some(host) = current.as_ref() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let layer = out.entry(host.clone()).or_insert_with(AppLayer::default);
        match key.trim() {
            "punct" => layer.punct = bool_value(value.trim()),
            "learning" => layer.learning = bool_value(value.trim()),
            _ => {}
        }
    }
    out
}

/// 读取 apps.toml;坏文件返回错误,由调用方记 issue。
pub fn apps_from_toml(text: &str) -> Result<BTreeMap<String, AppLayer>, String> {
    let layers: BTreeMap<String, AppLayer> =
        toml::from_str(text).map_err(|error| format!("apps.toml 解析失败: {error}"))?;
    Ok(layers
        .into_iter()
        .map(|(host, layer)| (host.trim().to_ascii_lowercase(), layer))
        .collect())
}

/// 渲染 `config.toml`。扩展命名空间原样写回,未知键已在上层拒绝。
pub fn render(cfg: &AppConfig) -> Result<String, String> {
    let bindings: BTreeMap<&str, &str> = cfg
        .bindings
        .iter()
        .map(|(combo, command)| (combo.as_str(), command.as_str()))
        .collect();
    let triggers: BTreeMap<&str, &str> = cfg
        .triggers
        .iter()
        .map(|(trigger, command)| (trigger.as_str(), command.as_str()))
        .collect();
    let file = SaveFile {
        page_size: cfg.page_size,
        chinese_punct: cfg.chinese_punct,
        fuzzy: if cfg.fuzzy.is_off() { "off" } else { "auto" },
        shift_toggle: cfg.shift_toggle,
        traditional: cfg.traditional,
        traditional_region: cfg.region.as_str(),
        scheme: cfg.scheme.as_str(),
        aux_code: cfg.aux,
        predict: cfg.predict,
        ui_layout: cfg.ui_layout.as_str(),
        ui_font: cfg.ui_font.as_str(),
        ui_accent: cfg.ui_accent,
        effects: cfg.effects,
        extensions: &cfg.extensions,
        panel_hotkey: &cfg.panel_hotkey,
        ocr_engine: cfg.ocr_backend.as_str(),
        update_channel: cfg.update_channel.as_str(),
        bindings,
        triggers,
        ext: &cfg.ext_settings,
    };
    let body = toml::to_string_pretty(&file).map_err(|error| format!("配置序列化失败: {error}"))?;
    Ok(format!(
        "# 掠影配置(TOML)。未知键会被拒绝并记进日志与服务日志。\n\
         # 按应用覆盖写在 apps.toml;扩展设置在各 [ext.<id>] 表。\n\n{body}"
    ))
}

/// 渲染 `apps.toml`:每个 exe 一节。
pub fn render_apps(apps: &BTreeMap<String, AppOverride>) -> Result<String, String> {
    let layers: BTreeMap<String, AppLayer> = apps
        .iter()
        .map(|(host, over)| {
            (
                host.clone(),
                AppLayer {
                    punct: over.punct,
                    learning: over.learning,
                },
            )
        })
        .collect();
    let body = toml::to_string_pretty(&layers)
        .map_err(|error| format!("按应用配置序列化失败: {error}"))?;
    Ok(format!(
        "# 掠影按应用配置(TOML)。节名是宿主进程的 exe 名(小写)。\n\n{body}"
    ))
}

#[derive(Serialize)]
struct SaveFile<'a> {    page_size: usize,
    chinese_punct: bool,
    fuzzy: &'static str,
    shift_toggle: bool,
    traditional: bool,
    traditional_region: &'static str,
    scheme: &'static str,
    aux_code: bool,
    predict: bool,
    ui_layout: &'static str,
    ui_font: &'static str,
    ui_accent: usize,
    effects: bool,
    extensions: &'a [String],
    panel_hotkey: &'a str,
    ocr_engine: &'static str,
    update_channel: &'static str,
    bindings: BTreeMap<&'a str, &'a str>,
    triggers: BTreeMap<&'a str, &'a str>,
    ext: &'a BTreeMap<String, BTreeMap<String, Value>>,
}

/// 按应用配置模板。
pub const APPS_TEMPLATE: &str = "\
# 掠影按应用配置:每个应用一节,节名是宿主进程的 exe 名(小写)。
# punct = false     本应用关闭中文标点(直出半角)
# learning = false  本应用不写入学习记录
# 没写的键跟随全局设置。示例(去掉行首 # 生效):
#[notepad.exe]
#punct = false
";

fn bool_value(value: &str) -> Option<bool> {
    match value {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

fn parse_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn pairs(value: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for item in value.split(',') {
        let item = item.trim();
        let Some((key, value)) = item.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() || value.is_empty() {
            continue;
        }
        out.entry(key.to_string()).or_insert_with(|| value.to_string());
    }
    out
}
