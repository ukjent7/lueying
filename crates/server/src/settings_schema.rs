//! 设置项的单一来源:配置键、类型、默认值、页面与控件。
//!
//! 设置窗的行、`lueying-settings --dump-config-schema` 输出的 JSON Schema
//! 与 `docs/settings.md` 的说明都从这里取元数据。新增设置先加一条
//! [`SettingDef`],再决定是否给它一个 [`UiDef`];这样配置解析、界面文案与
//! AI/脚本读取的 schema 不会各写一份。
//!
//! 这里不依赖 Windows:Linux 单测与 E2E 共用同一份表。

use serde_json::{Map, Value, json};

use crate::config::{AppConfig, Channel, OcrBackend, Region, UiFont, UiLayout};
use crate::settings_view::ids;
use lueying_features::SchemeId;

/// 设置窗左栏的页面。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageId {
    #[default]
    Input,
    Candidate,
    Extensions,
    Dictionary,
    About,
}

impl PageId {
    /// 左栏顺序。
    pub const ALL: [PageId; 5] = [
        Self::Input,
        Self::Candidate,
        Self::Extensions,
        Self::Dictionary,
        Self::About,
    ];

    /// 页面标题,同时是左栏文字。
    pub fn title(self) -> &'static str {
        match self {
            Self::Input => "输入",
            Self::Candidate => "候选",
            Self::Extensions => "扩展",
            Self::Dictionary => "词库",
            Self::About => "关于",
        }
    }

    /// 左栏控件的稳定 id。
    pub fn nav_id(self) -> &'static str {
        match self {
            Self::Input => "nav-input",
            Self::Candidate => "nav-candidate",
            Self::Extensions => "nav-extensions",
            Self::Dictionary => "nav-dictionary",
            Self::About => "nav-about",
        }
    }

    /// 从左栏控件 id 反查页面。
    pub fn from_nav(base: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|page| page.nav_id() == base)
    }
}

/// 配置默认值;与 `AppConfig::default` 由单测绑定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultValue {
    Bool(bool),
    Int(i64),
    Text(&'static str),
    List,
    Table,
}

/// 配置键的类型与取值范围,用于 JSON Schema。
#[derive(Clone, Copy, Debug)]
pub enum ValueType {
    Bool,
    Int { min: Option<i64>, max: Option<i64> },
    Text,
    Enum(&'static [&'static str]),
    List,
    Table,
}

/// 分段控件的一项:配置里存 `value`,界面显示 `label`。
#[derive(Clone, Copy, Debug)]
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

/// 设置行的控件类型;读写直接落在 [`AppConfig`] 上。
pub enum ControlDef {
    Toggle {
        id: &'static str,
        get: fn(&AppConfig) -> bool,
        set: fn(&mut AppConfig, bool),
    },
    Segment {
        id: &'static str,
        choices: &'static [Choice],
        /// 每格宽度(逻辑像素)。
        width: f32,
        /// 主题色一列要把预设色画在文字上。
        tinted: bool,
        get: fn(&AppConfig) -> usize,
        set: fn(&mut AppConfig, usize),
    },
}

impl ControlDef {
    pub fn id(&self) -> &'static str {
        match self {
            Self::Toggle { id, .. } | Self::Segment { id, .. } => id,
        }
    }
}

/// 一个设置在界面上的位置与可见条件。
pub struct UiDef {
    pub page: PageId,
    /// 搜索关键词;为空时用标题与说明。
    pub keywords: &'static str,
    pub control: ControlDef,
    /// 只有返回 true 时才显示这一行。
    pub visible: fn(&AppConfig) -> bool,
}

/// 一条设置:配置键 + 文档文案 + 可选的界面控件。
pub struct SettingDef {
    /// `config.toml` 里的键名。
    pub key: &'static str,
    /// 设置窗里的行标题,也是文档标题。
    pub title: &'static str,
    /// 一行说明,同时用于界面与文档。
    pub detail: &'static str,
    pub value: ValueType,
    pub default: DefaultValue,
    pub ui: Option<UiDef>,
}

/// 查找设置窗控件 id 对应的设置。
pub fn find(id: &str) -> Option<&'static SettingDef> {
    SETTINGS
        .iter()
        .find(|def| def.ui.as_ref().is_some_and(|ui| ui.control.id() == id))
}

/// 按页面列出要在界面上显示的设置。
pub fn on_page(page: PageId) -> impl Iterator<Item = &'static SettingDef> {
    SETTINGS
        .iter()
        .filter(move |def| def.ui.as_ref().is_some_and(|ui| ui.page == page))
}

/// 生成 `config.schema.json`:给脚本与 AI agent 读的配置说明。
pub fn config_schema_json() -> Value {
    let mut properties = Map::new();
    for def in SETTINGS {
        let mut property = Map::new();
        property.insert("title".to_string(), json!(def.title));
        property.insert("description".to_string(), json!(def.detail));
        match def.value {
            ValueType::Bool => {
                property.insert("type".to_string(), json!("boolean"));
            }
            ValueType::Int { min, max } => {
                property.insert("type".to_string(), json!("integer"));
                if let Some(min) = min {
                    property.insert("minimum".to_string(), json!(min));
                }
                if let Some(max) = max {
                    property.insert("maximum".to_string(), json!(max));
                }
            }
            ValueType::Text => {
                property.insert("type".to_string(), json!("string"));
            }
            ValueType::Enum(values) => {
                property.insert("type".to_string(), json!("string"));
                property.insert("enum".to_string(), json!(values));
            }
            ValueType::List => {
                property.insert("type".to_string(), json!("array"));
                property.insert("items".to_string(), json!({"type": "string"}));
            }
            ValueType::Table => {
                property.insert("type".to_string(), json!("object"));
                property.insert(
                    "additionalProperties".to_string(),
                    json!({"type": "string"}),
                );
            }
        }
        property.insert("default".to_string(), default_json(def.default));
        if let Some(ui) = &def.ui {
            property.insert("x-lueying-page".to_string(), json!(ui.page.title()));
            if !ui.keywords.is_empty() {
                property.insert("x-lueying-keywords".to_string(), json!(ui.keywords));
            }
        }
        properties.insert(def.key.to_string(), Value::Object(property));
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "掠影 config.toml",
        "description": "掠影输入法用户配置。未知键会被拒绝;按应用覆盖在 apps.toml。",
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
    })
}

/// 格式化后的 schema 文本;`--dump-config-schema` 直接打印。
pub fn config_schema_text() -> String {
    serde_json::to_string_pretty(&config_schema_json())
        .unwrap_or_else(|error| format!("配置 schema 序列化失败: {error}"))
}

fn default_json(default: DefaultValue) -> Value {
    match default {
        DefaultValue::Bool(value) => json!(value),
        DefaultValue::Int(value) => json!(value),
        DefaultValue::Text(value) => json!(value),
        DefaultValue::List => json!([]),
        DefaultValue::Table => json!({}),
    }
}

// ---- 配置读写函数:界面控件与 schema 共用 ----

fn aux_get(cfg: &AppConfig) -> bool {
    cfg.aux
}

fn aux_set(cfg: &mut AppConfig, value: bool) {
    cfg.aux = value;
}

fn punct_get(cfg: &AppConfig) -> bool {
    cfg.chinese_punct
}

fn punct_set(cfg: &mut AppConfig, value: bool) {
    cfg.chinese_punct = value;
}

fn shift_get(cfg: &AppConfig) -> bool {
    cfg.shift_toggle
}

fn shift_set(cfg: &mut AppConfig, value: bool) {
    cfg.shift_toggle = value;
}

fn traditional_get(cfg: &AppConfig) -> bool {
    cfg.traditional
}

fn traditional_set(cfg: &mut AppConfig, value: bool) {
    cfg.traditional = value;
}

fn effects_get(cfg: &AppConfig) -> bool {
    cfg.effects
}

fn effects_set(cfg: &mut AppConfig, value: bool) {
    cfg.effects = value;
}

fn predict_get(cfg: &AppConfig) -> bool {
    cfg.predict
}

fn predict_set(cfg: &mut AppConfig, value: bool) {
    cfg.predict = value;
}

fn fuzzy_get(cfg: &AppConfig) -> bool {
    !cfg.fuzzy.is_off()
}

fn fuzzy_set(cfg: &mut AppConfig, value: bool) {
    cfg.fuzzy = if value {
        lueying_engine::Fuzzy::auto()
    } else {
        lueying_engine::Fuzzy::off()
    };
}

fn scheme_get(cfg: &AppConfig) -> usize {
    SchemeId::ALL
        .iter()
        .position(|id| *id == cfg.scheme)
        .unwrap_or(0)
}

fn scheme_set(cfg: &mut AppConfig, index: usize) {
    if let Some(scheme) = SchemeId::ALL.get(index) {
        cfg.scheme = *scheme;
    }
}

fn region_get(cfg: &AppConfig) -> usize {
    cfg.region as usize
}

fn region_set(cfg: &mut AppConfig, index: usize) {
    cfg.region = Region::from_index(index);
}

fn page_size_get(cfg: &AppConfig) -> usize {
    PAGE_SIZE_VALUES
        .iter()
        .position(|size| *size == cfg.page_size)
        .unwrap_or(1)
}

fn page_size_set(cfg: &mut AppConfig, index: usize) {
    if let Some(size) = PAGE_SIZE_VALUES.get(index) {
        cfg.page_size = *size;
    }
}

fn layout_get(cfg: &AppConfig) -> usize {
    usize::from(cfg.ui_layout == UiLayout::Vertical)
}

fn layout_set(cfg: &mut AppConfig, index: usize) {
    cfg.ui_layout = UiLayout::from_index(index);
}

fn font_get(cfg: &AppConfig) -> usize {
    FONT_VALUES
        .iter()
        .position(|font| *font == cfg.ui_font)
        .unwrap_or(1)
}

fn font_set(cfg: &mut AppConfig, index: usize) {
    cfg.ui_font = UiFont::from_index(index);
}

fn accent_get(cfg: &AppConfig) -> usize {
    cfg.ui_accent.min(ACCENT_CHOICES.len().saturating_sub(1))
}

fn accent_set(cfg: &mut AppConfig, index: usize) {
    if index < ACCENT_CHOICES.len() {
        cfg.ui_accent = index;
    }
}

fn ocr_get(cfg: &AppConfig) -> usize {
    usize::from(cfg.ocr_backend == OcrBackend::Ppocr)
}

fn ocr_set(cfg: &mut AppConfig, index: usize) {
    cfg.ocr_backend = OcrBackend::from_index(index);
}

fn channel_get(cfg: &AppConfig) -> usize {
    usize::from(cfg.update_channel == Channel::Beta)
}

fn channel_set(cfg: &mut AppConfig, index: usize) {
    cfg.update_channel = Channel::from_index(index);
}

fn always(_: &AppConfig) -> bool {
    true
}

fn when_traditional(cfg: &AppConfig) -> bool {
    cfg.traditional
}

// ---- 选项表 ----

const SCHEME_CHOICES: &[Choice] = &[
    Choice {
        value: "full",
        label: "全拼",
    },
    Choice {
        value: "xiaohe",
        label: "小鹤",
    },
    Choice {
        value: "ziranma",
        label: "自然码",
    },
    Choice {
        value: "mspy",
        label: "微软",
    },
    Choice {
        value: "sogou",
        label: "搜狗",
    },
];
const REGION_CHOICES: &[Choice] = &[
    Choice {
        value: "general",
        label: "通用",
    },
    Choice {
        value: "tw",
        label: "台湾",
    },
    Choice {
        value: "hk",
        label: "香港",
    },
];
const PAGE_SIZE_VALUES: [usize; 3] = [3, 5, 9];
const PAGE_SIZE_CHOICES: &[Choice] = &[
    Choice {
        value: "3",
        label: "3",
    },
    Choice {
        value: "5",
        label: "5",
    },
    Choice {
        value: "9",
        label: "9",
    },
];
const LAYOUT_CHOICES: &[Choice] = &[
    Choice {
        value: "horizontal",
        label: "横排",
    },
    Choice {
        value: "vertical",
        label: "竖排",
    },
];
const FONT_VALUES: [UiFont; 3] = [UiFont::Small, UiFont::Standard, UiFont::Large];
const FONT_CHOICES: &[Choice] = &[
    Choice {
        value: "small",
        label: "小",
    },
    Choice {
        value: "standard",
        label: "标准",
    },
    Choice {
        value: "large",
        label: "大",
    },
];
const ACCENT_CHOICES: &[Choice] = &[
    Choice {
        value: "0",
        label: "蓝",
    },
    Choice {
        value: "1",
        label: "青",
    },
    Choice {
        value: "2",
        label: "绿",
    },
    Choice {
        value: "3",
        label: "紫",
    },
    Choice {
        value: "4",
        label: "红",
    },
    Choice {
        value: "5",
        label: "橙",
    },
];
const OCR_CHOICES: &[Choice] = &[
    Choice {
        value: "system",
        label: "系统",
    },
    Choice {
        value: "ppocr",
        label: "PP-OCRv6",
    },
];
const CHANNEL_CHOICES: &[Choice] = &[
    Choice {
        value: "stable",
        label: "稳定",
    },
    Choice {
        value: "beta",
        label: "测试",
    },
];

const fn toggle(
    id: &'static str,
    get: fn(&AppConfig) -> bool,
    set: fn(&mut AppConfig, bool),
) -> ControlDef {
    ControlDef::Toggle { id, get, set }
}

const fn segment(
    id: &'static str,
    choices: &'static [Choice],
    width: f32,
    tinted: bool,
    get: fn(&AppConfig) -> usize,
    set: fn(&mut AppConfig, usize),
) -> ControlDef {
    ControlDef::Segment {
        id,
        choices,
        width,
        tinted,
        get,
        set,
    }
}

/// 全部配置键。界面能改的带 `ui`,其余只用于 JSON Schema 与文档。
pub static SETTINGS: &[SettingDef] = &[
    SettingDef {
        key: "scheme",
        title: "输入方案",
        detail: "全拼或双拼,共享词库",
        value: ValueType::Enum(&["full", "xiaohe", "ziranma", "mspy", "sogou"]),
        default: DefaultValue::Text("full"),
        ui: Some(UiDef {
            page: PageId::Input,
            keywords: "拼音 全拼 双拼 小鹤 自然码 微软 搜狗",
            control: segment(
                ids::SCHEME,
                SCHEME_CHOICES,
                44.0,
                false,
                scheme_get,
                scheme_set,
            ),
            visible: always,
        }),
    },
    SettingDef {
        key: "aux_code",
        title: "辅助码",
        detail: "拼音后按 ` 再打 1–2 位五笔码",
        value: ValueType::Bool,
        default: DefaultValue::Bool(false),
        ui: Some(UiDef {
            page: PageId::Input,
            keywords: "五笔 辅码",
            control: toggle(ids::AUX, aux_get, aux_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "chinese_punct",
        title: "中文标点",
        detail: "按中文习惯输出 ，。；：“”",
        value: ValueType::Bool,
        default: DefaultValue::Bool(true),
        ui: Some(UiDef {
            page: PageId::Input,
            keywords: "标点 全角 半角",
            control: toggle(ids::CHINESE_PUNCT, punct_get, punct_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "shift_toggle",
        title: "Shift 单击切中英",
        detail: "按住 Shift 输入大写，单击切换",
        value: ValueType::Bool,
        default: DefaultValue::Bool(true),
        ui: Some(UiDef {
            page: PageId::Input,
            keywords: "中英 切换 大写",
            control: toggle(ids::SHIFT_TOGGLE, shift_get, shift_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "traditional",
        title: "输出繁体",
        detail: "候选与上屏使用繁体",
        value: ValueType::Bool,
        default: DefaultValue::Bool(false),
        ui: Some(UiDef {
            page: PageId::Input,
            keywords: "繁体 简体 台湾 香港",
            control: toggle(ids::TRADITIONAL, traditional_get, traditional_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "traditional_region",
        title: "字形地区",
        detail: "台湾、香港按当地常用字形",
        value: ValueType::Enum(&["general", "tw", "hk"]),
        default: DefaultValue::Text("general"),
        ui: Some(UiDef {
            page: PageId::Input,
            keywords: "繁体 字形 台湾 香港",
            control: segment(
                ids::REGION,
                REGION_CHOICES,
                44.0,
                false,
                region_get,
                region_set,
            ),
            visible: when_traditional,
        }),
    },
    SettingDef {
        key: "page_size",
        title: "每页候选数",
        detail: "按屏幕宽度选择",
        value: ValueType::Int {
            min: Some(3),
            max: Some(9),
        },
        default: DefaultValue::Int(5),
        ui: Some(UiDef {
            page: PageId::Candidate,
            keywords: "候选 数量 每页",
            control: segment(
                ids::PAGE_SIZE,
                PAGE_SIZE_CHOICES,
                34.0,
                false,
                page_size_get,
                page_size_set,
            ),
            visible: always,
        }),
    },
    SettingDef {
        key: "ui_layout",
        title: "候选排列",
        detail: "横排挤一行,竖排一行一条",
        value: ValueType::Enum(&["horizontal", "vertical"]),
        default: DefaultValue::Text("horizontal"),
        ui: Some(UiDef {
            page: PageId::Candidate,
            keywords: "候选 横排 竖排 排列",
            control: segment(
                ids::LAYOUT,
                LAYOUT_CHOICES,
                44.0,
                false,
                layout_get,
                layout_set,
            ),
            visible: always,
        }),
    },
    SettingDef {
        key: "ui_font",
        title: "候选字号",
        detail: "只缩放候选行,拼音行不变",
        value: ValueType::Enum(&["small", "standard", "large"]),
        default: DefaultValue::Text("standard"),
        ui: Some(UiDef {
            page: PageId::Candidate,
            keywords: "候选 字体 字号 大小",
            control: segment(ids::FONT, FONT_CHOICES, 44.0, false, font_get, font_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "ui_accent",
        title: "主题色",
        detail: "候选窗的强调色,高对比度下跟随系统",
        value: ValueType::Enum(&["0", "1", "2", "3", "4", "5"]),
        default: DefaultValue::Int(0),
        ui: Some(UiDef {
            page: PageId::Candidate,
            keywords: "主题 颜色 强调色 外观",
            control: segment(
                ids::ACCENT,
                ACCENT_CHOICES,
                34.0,
                true,
                accent_get,
                accent_set,
            ),
            visible: always,
        }),
    },
    SettingDef {
        key: "effects",
        title: "用户效果",
        detail: "SkSL 装饰层,放进去即生效",
        value: ValueType::Bool,
        default: DefaultValue::Bool(true),
        ui: Some(UiDef {
            page: PageId::Candidate,
            keywords: "效果 动画 外观 SkSL",
            control: toggle(ids::EFFECTS_TOGGLE, effects_get, effects_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "predict",
        title: "下一词联想",
        detail: "按本机上屏词序补全",
        value: ValueType::Bool,
        default: DefaultValue::Bool(true),
        ui: Some(UiDef {
            page: PageId::Candidate,
            keywords: "联想 预测 下一词",
            control: toggle(ids::PREDICT, predict_get, predict_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "fuzzy",
        title: "自动模糊音",
        detail: "按输入习惯学习模糊音",
        value: ValueType::Enum(&["auto", "off"]),
        default: DefaultValue::Text("auto"),
        ui: Some(UiDef {
            page: PageId::Candidate,
            keywords: "模糊音 容错 拼音",
            control: toggle(ids::FUZZY, fuzzy_get, fuzzy_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "ocr_engine",
        title: "截图识别",
        detail: "系统 OCR 零依赖;PP-OCRv6 首次识别时加载",
        value: ValueType::Enum(&["system", "ppocr"]),
        default: DefaultValue::Text("system"),
        ui: Some(UiDef {
            page: PageId::Extensions,
            keywords: "OCR 截图 识别 模型",
            control: segment(ids::OCR, OCR_CHOICES, 60.0, false, ocr_get, ocr_set),
            visible: always,
        }),
    },
    SettingDef {
        key: "update_channel",
        title: "更新渠道",
        detail: "稳定只认正式版;测试跟进预发布 Release",
        value: ValueType::Enum(&["stable", "beta"]),
        default: DefaultValue::Text("stable"),
        ui: Some(UiDef {
            page: PageId::About,
            keywords: "更新 升级 测试版 稳定版",
            control: segment(
                ids::CHANNEL,
                CHANNEL_CHOICES,
                44.0,
                false,
                channel_get,
                channel_set,
            ),
            visible: always,
        }),
    },
    SettingDef {
        key: "extensions",
        title: "运行期脚本",
        detail: "启用的扩展 id 列表;在「扩展」页选择与排序",
        value: ValueType::List,
        default: DefaultValue::List,
        ui: None,
    },
    SettingDef {
        key: "bindings",
        title: "扩展热键",
        detail: "组合键 → 命令 id;用 `lueying ext` 或手工编辑维护",
        value: ValueType::Table,
        default: DefaultValue::Table,
        ui: None,
    },
    SettingDef {
        key: "triggers",
        title: "动作触发词",
        detail: "触发词 → 命令 id;整串匹配后出现动作候选",
        value: ValueType::Table,
        default: DefaultValue::Table,
        ui: None,
    },
    SettingDef {
        key: "panel_hotkey",
        title: "命令面板热键",
        detail: "默认 Ctrl+Alt+Shift+Space;写 off 关闭",
        value: ValueType::Text,
        default: DefaultValue::Text("Ctrl+Alt+Shift+Space"),
        ui: None,
    },
    SettingDef {
        key: "commit_channel",
        title: "对外上屏通道",
        detail: "默认关闭;开启后本地工具可凭令牌借用上屏能力",
        value: ValueType::Bool,
        default: DefaultValue::Bool(false),
        ui: None,
    },
    SettingDef {
        key: "settled_rerank_provider",
        title: "停顿重排提供者",
        detail: "写 bypass 走旁路进程;缺省用内置 25M 模型",
        value: ValueType::Text,
        default: DefaultValue::Text(""),
        ui: None,
    },
    SettingDef {
        key: "bypass_exe",
        title: "旁路进程路径",
        detail: "停顿重排提供者的可执行文件",
        value: ValueType::Text,
        default: DefaultValue::Text(""),
        ui: None,
    },
    SettingDef {
        key: "ext",
        title: "扩展设置",
        detail: "[ext.<id>] 命名空间;由扩展清单声明的 schema 校验",
        value: ValueType::Table,
        default: DefaultValue::Table,
        ui: None,
    },
];

#[cfg(test)]
#[path = "settings_schema_tests.rs"]
mod tests;
