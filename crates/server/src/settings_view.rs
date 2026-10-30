//! 设置窗的页面模型:配置铺成一页,点控件改配置。
//!
//! 这一层不碰 Windows:页面长什么样、点一下改哪个配置、Tab 走到哪,都在
//! 这里;平台侧只管窗口、绘制与文件对话框。Linux E2E 因此能直接断言
//! 设置页的结构与交互结果。

use lueying_engine::Fuzzy;
use lueying_features::SchemeId;
use lueying_protocol::EngineStatus;
use lueying_render::ui::{Button, ButtonKind, Control, Group, Id, Page, Row, SegmentOption};

use crate::accents::ACCENTS;
use crate::config::{AppConfig, Channel, OcrBackend, Region, UiFont, UiLayout};

/// 控件 id 的前缀。分段组用「前缀 + 格下标」,其余控件用「前缀 + 0」。
pub mod ids {
    pub const UPDATE: &str = "update";
    pub const SCHEME: &str = "scheme";
    pub const AUX: &str = "aux";
    pub const CHINESE_PUNCT: &str = "chinese-punct";
    pub const SHIFT_TOGGLE: &str = "shift-toggle";
    pub const TRADITIONAL: &str = "traditional";
    pub const REGION: &str = "region";
    pub const PAGE_SIZE: &str = "page-size";
    pub const LAYOUT: &str = "layout";
    pub const FONT: &str = "font";
    pub const ACCENT: &str = "accent";
    pub const EFFECTS: &str = "effects";
    pub const EFFECTS_TOGGLE: &str = "effects-toggle";
    pub const SCRIPTS: &str = "scripts";
    pub const SCRIPTS_DIR: &str = "scripts-dir";
    pub const OCR: &str = "ocr";
    pub const OCR_DIR: &str = "ocr-dir";
    pub const PREDICT: &str = "predict";
    pub const RESET_FUZZY: &str = "reset-fuzzy";
    pub const FUZZY: &str = "fuzzy";
    pub const PHRASES: &str = "phrases";
    pub const PUNCT: &str = "punct";
    pub const APPS: &str = "apps";
    pub const BACKUP_EXPORT: &str = "backup-export";
    pub const BACKUP_IMPORT: &str = "backup-import";
    pub const IMPORT_USER: &str = "import-user";
    pub const EXPORT_USER: &str = "export-user";
    pub const CLEAR_USER: &str = "clear-user";
    pub const DIAGNOSTICS: &str = "diagnostics";
    pub const CHANNEL: &str = "channel";
    pub const CLOSE: &str = "close";
    pub const SAVE: &str = "save";
}

/// 提示的种类:决定页脚那行字是什么颜色。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Notice {
    None,
    Info,
    Success,
    Error,
}

/// 更新状态:设置窗只关心按钮上写什么、能不能点。
#[derive(Clone, Debug)]
pub enum UpdateState {
    /// 还没查过。
    Idle,
    Checking,
    Latest,
    NoRelease,
    /// 有新版可装,版本号在这里。
    Available(String),
    Failed(String),
}

/// 页面里要用的颜色,由平台侧按主题算好传进来。
#[derive(Clone, Debug)]
pub struct PageColors {
    /// 普通提示文字的颜色。
    pub notice_info: u32,
    /// 成功提示的颜色。
    pub notice_success: u32,
    /// 失败提示的颜色。
    pub notice_error: u32,
    /// 主题色分段里每一格的颜色,顺序同 [`ACCENTS`]。
    pub accents: Vec<u32>,
}

/// 平台侧要去做的事:改配置之外的动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Save,
    Close,
    CheckUpdate,
    InstallUpdate(String),
    OpenEffects,
    ChooseScript,
    OpenScripts,
    OpenOcrModels,
    OpenPhrases,
    OpenPunct,
    OpenApps,
    BackupExport,
    BackupImport,
    ImportUser,
    ExportUser,
    ClearUser,
    Diagnostics,
    ResetFuzzy,
}

/// 已安装的运行期脚本包;设置页只读清单,不加载 JS。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptEntry {
    pub id: String,
    pub name: String,
}

/// 设置窗的状态:配置 + 服务端回来的东西 + 提示。
pub struct Settings {
    pub cfg: AppConfig,
    /// 配置文件路径;没有就存不了。
    pub path: Option<std::path::PathBuf>,
    /// 模糊组当前适应次数,来自常驻 Server。
    pub groups: Vec<(String, u32)>,
    /// 引擎资源状态;服务未运行时是 None。
    pub status: Option<EngineStatus>,
    /// 页脚那行提示文字。
    pub message: String,
    pub notice: Notice,
    /// 清空词库要点两次,防误触。
    pub confirm_clear: bool,
    /// 安装目录里的版本号。
    pub current: String,
    pub updates: UpdateState,
    /// 用户效果目录的说明文字。
    pub effects_detail: String,
    /// 已安装的运行期脚本包;平台侧读目录后填入。
    pub scripts: Vec<ScriptEntry>,
    /// PP-OCRv6 模型是否齐全;平台侧检查目录后填入。
    pub ppocr_ready: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            cfg: AppConfig::default(),
            path: None,
            groups: Vec::new(),
            status: None,
            message: String::new(),
            notice: Notice::None,
            confirm_clear: false,
            current: String::new(),
            updates: UpdateState::Idle,
            effects_detail: "SkSL 装饰层".to_string(),
            scripts: Vec::new(),
            ppocr_ready: false,
        }
    }
}

impl Settings {
    /// 换掉配置。
    pub fn with_config(cfg: AppConfig) -> Self {
        Self {
            cfg,
            ..Self::default()
        }
    }

    /// 清掉提示与清空确认态。
    pub fn clear_notice(&mut self) {
        self.notice = Notice::None;
        self.message.clear();
        self.confirm_clear = false;
    }

    /// 记一条提示。除了「再点一次确认清空」,任何提示都结束清空确认态。
    pub fn say(&mut self, notice: Notice, message: impl Into<String>) {
        if notice != Notice::Info {
            self.confirm_clear = false;
        }
        self.notice = notice;
        self.message = message.into();
    }

    /// 页面:把配置与服务端状态铺成一页。
    pub fn page(&self, colors: &PageColors) -> Page {
        Page {
            title: "掠影".to_string(),
            subtitle: format!("拼音输入法 · v{}", self.current),
            header_button: Some(self.update_button()),
            groups: self.groups_page(colors),
            notice: self.notice_text(),
            notice_tint: self.notice_tint(colors),
            footer_buttons: vec![
                Button {
                    id: Id::new(ids::CLOSE, 0),
                    text: "关闭".to_string(),
                    kind: ButtonKind::Secondary,
                    enabled: true,
                },
                Button {
                    id: Id::new(ids::SAVE, 0),
                    text: "保存".to_string(),
                    kind: ButtonKind::Primary,
                    enabled: true,
                },
            ],
        }
    }

    /// 四个分组。
    fn groups_page(&self, colors: &PageColors) -> Vec<Group> {
        vec![
            Group {
                title: "输入".to_string(),
                rows: vec![
                    Row {
                        label: "输入方案".to_string(),
                        detail: "全拼或双拼,共享词库".to_string(),
                        control: Control::Segment {
                            id: Id::new(ids::SCHEME, 0),
                            options: SchemeId::ALL
                                .iter()
                                .map(|id| SegmentOption::new(id.name()))
                                .collect(),
                            selected: self.selected(ids::SCHEME),
                            width: 44.0,
                        },
                    },
                    Row {
                        label: "辅助码".to_string(),
                        detail: "拼音后按 ` 再打 1–2 位五笔码".to_string(),
                        control: Control::Toggle {
                            id: Id::new(ids::AUX, 0),
                            on: self.cfg.aux,
                        },
                    },
                    Row {
                        label: "中文标点".to_string(),
                        detail: "按中文习惯输出 ，。；：“”".to_string(),
                        control: Control::Toggle {
                            id: Id::new(ids::CHINESE_PUNCT, 0),
                            on: self.cfg.chinese_punct,
                        },
                    },
                    Row {
                        label: "Shift 单击切中英".to_string(),
                        detail: "按住 Shift 输入大写，单击切换".to_string(),
                        control: Control::Toggle {
                            id: Id::new(ids::SHIFT_TOGGLE, 0),
                            on: self.cfg.shift_toggle,
                        },
                    },
                    Row {
                        label: "输出繁体".to_string(),
                        detail: "候选与上屏使用繁体".to_string(),
                        control: Control::Toggle {
                            id: Id::new(ids::TRADITIONAL, 0),
                            on: self.cfg.traditional,
                        },
                    },
                    Row {
                        label: "字形地区".to_string(),
                        detail: "台湾、香港按当地常用字形".to_string(),
                        control: Control::Segment {
                            id: Id::new(ids::REGION, 0),
                            options: Region::NAMES
                                .iter()
                                .map(|name| SegmentOption::new(*name))
                                .collect(),
                            selected: self.selected(ids::REGION),
                            width: 44.0,
                        },
                    },
                ],
            },
            Group {
                title: "候选".to_string(),
                rows: vec![
                    Row {
                        label: "每页候选数".to_string(),
                        detail: "按屏幕宽度选择".to_string(),
                        control: Control::Segment {
                            id: Id::new(ids::PAGE_SIZE, 0),
                            options: [3usize, 5, 9]
                                .iter()
                                .map(|n| SegmentOption::new(n.to_string()))
                                .collect(),
                            selected: self.selected(ids::PAGE_SIZE),
                            width: 34.0,
                        },
                    },
                    Row {
                        label: "候选排列".to_string(),
                        detail: "横排挤一行,竖排一行一条".to_string(),
                        control: Control::Segment {
                            id: Id::new(ids::LAYOUT, 0),
                            options: UiLayout::NAMES
                                .iter()
                                .map(|name| SegmentOption::new(*name))
                                .collect(),
                            selected: self.selected(ids::LAYOUT),
                            width: 44.0,
                        },
                    },
                    Row {
                        label: "候选字号".to_string(),
                        detail: "只缩放候选行,拼音行不变".to_string(),
                        control: Control::Segment {
                            id: Id::new(ids::FONT, 0),
                            options: UiFont::NAMES
                                .iter()
                                .map(|name| SegmentOption::new(*name))
                                .collect(),
                            selected: self.selected(ids::FONT),
                            width: 44.0,
                        },
                    },
                    Row {
                        label: "主题色".to_string(),
                        detail: "候选窗的强调色,高对比度下跟随系统".to_string(),
                        control: Control::Segment {
                            id: Id::new(ids::ACCENT, 0),
                            options: ACCENTS
                                .iter()
                                .enumerate()
                                .map(|(index, accent)| {
                                    let tint = colors.accents.get(index).copied().unwrap_or(0);
                                    SegmentOption::tinted(accent.name, tint)
                                })
                                .collect(),
                            selected: self.selected(ids::ACCENT),
                            width: 34.0,
                        },
                    },
                    Row {
                        label: "用户效果".to_string(),
                        detail: self.effects_detail.clone(),
                        control: Control::ButtonsToggle {
                            items: vec![Button {
                                id: Id::new(ids::EFFECTS, 0),
                                text: "目录".to_string(),
                                kind: ButtonKind::Secondary,
                                enabled: true,
                            }],
                            toggle: Id::new(ids::EFFECTS_TOGGLE, 0),
                            toggle_on: self.cfg.effects,
                        },
                    },
                    Row {
                        label: "下一词联想".to_string(),
                        detail: "按本机上屏词序补全".to_string(),
                        control: Control::Toggle {
                            id: Id::new(ids::PREDICT, 0),
                            on: self.cfg.predict,
                        },
                    },
                    Row {
                        label: "自动模糊音".to_string(),
                        detail: self.fuzzy_detail(),
                        control: Control::ButtonsToggle {
                            items: vec![Button {
                                id: Id::new(ids::RESET_FUZZY, 0),
                                text: "重置".to_string(),
                                kind: ButtonKind::Ghost,
                                enabled: self.reset_enabled(),
                            }],
                            toggle: Id::new(ids::FUZZY, 0),
                            toggle_on: !self.cfg.fuzzy.is_off(),
                        },
                    },
                ],
            },
            Group {
                title: "扩展".to_string(),
                rows: vec![
                    Row {
                        label: "运行期脚本".to_string(),
                        detail: self.script_detail(),
                        control: Control::Buttons {
                            items: vec![
                                Button {
                                    id: Id::new(ids::SCRIPTS, 0),
                                    text: "选择".to_string(),
                                    kind: ButtonKind::Secondary,
                                    enabled: true,
                                },
                                Button {
                                    id: Id::new(ids::SCRIPTS_DIR, 0),
                                    text: "目录".to_string(),
                                    kind: ButtonKind::Secondary,
                                    enabled: true,
                                },
                            ],
                        },
                    },
                    Row {
                        label: "截图识别".to_string(),
                        detail: self.ocr_detail(),
                        control: Control::Segment {
                            id: Id::new(ids::OCR, 0),
                            options: OcrBackend::NAMES
                                .iter()
                                .map(|name| SegmentOption::new(*name))
                                .collect(),
                            selected: self.selected(ids::OCR),
                            width: 60.0,
                        },
                    },
                    Row {
                        label: "识别模型".to_string(),
                        detail: "放入 det.onnx、rec.onnx、dict.txt".to_string(),
                        control: Control::Buttons {
                            items: vec![Button {
                                id: Id::new(ids::OCR_DIR, 0),
                                text: "目录".to_string(),
                                kind: ButtonKind::Secondary,
                                enabled: true,
                            }],
                        },
                    },
                ],
            },
            Group {
                title: "词库".to_string(),
                rows: vec![
                    Row {
                        label: "自定义短语".to_string(),
                        detail: "编辑 phrases.txt，保存后重载".to_string(),
                        control: Control::Buttons {
                            items: vec![Button {
                                id: Id::new(ids::PHRASES, 0),
                                text: "编辑".to_string(),
                                kind: ButtonKind::Secondary,
                                enabled: true,
                            }],
                        },
                    },
                    Row {
                        label: "自定义标点".to_string(),
                        detail: "编辑 punct.txt，映射半角符号的输出".to_string(),
                        control: Control::Buttons {
                            items: vec![Button {
                                id: Id::new(ids::PUNCT, 0),
                                text: "编辑".to_string(),
                                kind: ButtonKind::Secondary,
                                enabled: true,
                            }],
                        },
                    },
                    Row {
                        label: "按应用配置".to_string(),
                        detail: "编辑 apps.ini，单个应用关标点或学习".to_string(),
                        control: Control::Buttons {
                            items: vec![Button {
                                id: Id::new(ids::APPS, 0),
                                text: "编辑".to_string(),
                                kind: ButtonKind::Secondary,
                                enabled: true,
                            }],
                        },
                    },
                    Row {
                        label: "配置备份".to_string(),
                        detail: "配置、短语、标点、按应用与词库打包成一份文本".to_string(),
                        control: Control::Buttons {
                            items: vec![
                                Button {
                                    id: Id::new(ids::BACKUP_EXPORT, 0),
                                    text: "导出".to_string(),
                                    kind: ButtonKind::Secondary,
                                    enabled: true,
                                },
                                Button {
                                    id: Id::new(ids::BACKUP_IMPORT, 0),
                                    text: "导入".to_string(),
                                    kind: ButtonKind::Secondary,
                                    enabled: true,
                                },
                            ],
                        },
                    },
                    Row {
                        label: "用户词库".to_string(),
                        detail: "导入、导出或清空本机学习记录".to_string(),
                        control: Control::Buttons {
                            items: vec![
                                Button {
                                    id: Id::new(ids::IMPORT_USER, 0),
                                    text: "导入".to_string(),
                                    kind: ButtonKind::Secondary,
                                    enabled: true,
                                },
                                Button {
                                    id: Id::new(ids::EXPORT_USER, 0),
                                    text: "导出".to_string(),
                                    kind: ButtonKind::Secondary,
                                    enabled: true,
                                },
                                Button {
                                    id: Id::new(ids::CLEAR_USER, 0),
                                    text: if self.confirm_clear {
                                        "确认清空"
                                    } else {
                                        "清空"
                                    }
                                    .to_string(),
                                    kind: if self.confirm_clear {
                                        ButtonKind::Danger
                                    } else {
                                        ButtonKind::Ghost
                                    },
                                    enabled: true,
                                },
                            ],
                        },
                    },
                ],
            },
            Group {
                title: "关于".to_string(),
                rows: vec![
                    Row {
                        label: "引擎状态".to_string(),
                        detail: status_detail(&self.status),
                        control: Control::None,
                    },
                    Row {
                        label: "更新渠道".to_string(),
                        detail: "测试版会跟进预发布 Release".to_string(),
                        control: Control::Segment {
                            id: Id::new(ids::CHANNEL, 0),
                            options: Channel::NAMES
                                .iter()
                                .map(|name| SegmentOption::new(*name))
                                .collect(),
                            selected: self.selected(ids::CHANNEL),
                            width: 44.0,
                        },
                    },
                    Row {
                        label: "诊断信息".to_string(),
                        detail: "版本、配置与服务日志导出成一份文本".to_string(),
                        control: Control::Buttons {
                            items: vec![Button {
                                id: Id::new(ids::DIAGNOSTICS, 0),
                                text: "导出".to_string(),
                                kind: ButtonKind::Secondary,
                                enabled: true,
                            }],
                        },
                    },
                ],
            },
        ]
    }

    /// 头部右侧的更新按钮。
    fn update_button(&self) -> Button {
        let (text, kind, enabled) = match &self.updates {
            UpdateState::Idle => ("检查更新".to_string(), ButtonKind::Ghost, true),
            UpdateState::Checking => ("检查中".to_string(), ButtonKind::Ghost, false),
            UpdateState::Latest => ("已是最新".to_string(), ButtonKind::Ghost, true),
            UpdateState::NoRelease => ("检查更新".to_string(), ButtonKind::Ghost, true),
            UpdateState::Available(version) => {
                (format!("安装 {version}"), ButtonKind::Primary, true)
            }
            UpdateState::Failed(_) => ("检查失败 · 重试".to_string(), ButtonKind::Ghost, true),
        };
        Button {
            id: Id::new(ids::UPDATE, 0),
            text,
            kind,
            enabled,
        }
    }

    /// 页脚那行字:有话说就说,没话说就提醒保存。
    fn notice_text(&self) -> String {
        if !self.message.is_empty() {
            return self.message.clone();
        }
        if let UpdateState::Failed(error) = &self.updates {
            return format!("检查更新失败: {error}");
        }
        "修改后点保存生效".to_string()
    }

    /// 页脚那行字的颜色。
    fn notice_tint(&self, colors: &PageColors) -> u32 {
        match self.notice {
            Notice::Success => colors.notice_success,
            Notice::Error => colors.notice_error,
            Notice::None | Notice::Info => colors.notice_info,
        }
    }

    /// 模糊音那行的说明:已经适应了哪几组就写哪几组。
    fn fuzzy_detail(&self) -> String {
        let adapted = self
            .groups
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(name, count)| format!("{name} ×{count}"))
            .collect::<Vec<_>>()
            .join(" · ");
        if adapted.is_empty() {
            "按输入习惯学习模糊音".to_string()
        } else {
            adapted
        }
    }

    /// 运行期脚本那行的说明:当前包,或者怎么开始。
    fn script_detail(&self) -> String {
        match self.cfg.extensions.as_slice() {
            [] => {
                if self.scripts.is_empty() {
                    "未安装扩展;点「目录」放入扩展包".to_string()
                } else {
                    format!("未启用 · 已安装 {} 个", self.scripts.len())
                }
            }
            [id] => match self.scripts.iter().find(|script| &script.id == id) {
                Some(script) => format!("{} · {}", script.name, script.id),
                None => format!("{id} · 未安装"),
            },
            ids => format!("已启用 {} 个 · {}", ids.len(), ids.join(", ")),
        }
    }

    /// 截图识别那行的说明:选的引擎与模型是否可用。
    fn ocr_detail(&self) -> String {
        match (self.cfg.ocr_backend, self.ppocr_ready) {
            (OcrBackend::Ppocr, true) => "随包 PP-OCRv6 · 首次识别加载".to_string(),
            (OcrBackend::Ppocr, false) => "模型未安装;点「目录」放入三个文件".to_string(),
            (OcrBackend::System, true) => "系统 OCR;也可切到 PP-OCRv6".to_string(),
            (OcrBackend::System, false) => "系统 OCR;PP-OCRv6 模型未安装".to_string(),
        }
    }

    /// 选择运行期脚本;`None` 关闭。下标越界按关闭处理。
    pub fn select_script(&mut self, index: Option<usize>) {
        match index.and_then(|index| self.scripts.get(index)) {
            Some(script) => self.cfg.extensions = vec![script.id.clone()],
            None => self.cfg.extensions.clear(),
        }
        self.clear_notice();
    }

    /// 当前启用的包在列表里的下标;没启用或启用了多个返回 None。
    pub fn script_selected(&self) -> Option<usize> {
        let [id] = self.cfg.extensions.as_slice() else {
            return None;
        };
        self.scripts.iter().position(|script| &script.id == id)
    }

    /// 模糊音统计有没有内容可重置。
    pub fn reset_enabled(&self) -> bool {
        self.groups.iter().any(|(_, count)| *count > 0)
    }

    /// 点一下控件:能改配置的直接改,改不了的交给平台侧。
    pub fn activate(&mut self, id: &Id) -> Option<Action> {
        match id.base {
            ids::AUX => {
                self.cfg.aux = !self.cfg.aux;
                self.clear_notice();
                None
            }
            ids::CHINESE_PUNCT => {
                self.cfg.chinese_punct = !self.cfg.chinese_punct;
                self.clear_notice();
                None
            }
            ids::SHIFT_TOGGLE => {
                self.cfg.shift_toggle = !self.cfg.shift_toggle;
                self.clear_notice();
                None
            }
            ids::TRADITIONAL => {
                self.cfg.traditional = !self.cfg.traditional;
                self.clear_notice();
                None
            }
            ids::EFFECTS_TOGGLE => {
                self.cfg.effects = !self.cfg.effects;
                self.clear_notice();
                None
            }
            ids::PREDICT => {
                self.cfg.predict = !self.cfg.predict;
                self.clear_notice();
                None
            }
            ids::FUZZY => {
                self.cfg.fuzzy = if self.cfg.fuzzy.is_off() {
                    Fuzzy::auto()
                } else {
                    Fuzzy::off()
                };
                self.clear_notice();
                None
            }
            ids::SCHEME => {
                if let Some(scheme) = SchemeId::ALL.get(id.index) {
                    self.cfg.scheme = *scheme;
                    self.clear_notice();
                }
                None
            }
            ids::REGION => {
                self.cfg.region = Region::from_index(id.index);
                self.clear_notice();
                None
            }
            ids::PAGE_SIZE => {
                if let Some(size) = [3usize, 5, 9].get(id.index) {
                    self.cfg.page_size = *size;
                    self.clear_notice();
                }
                None
            }
            ids::LAYOUT => {
                self.cfg.ui_layout = UiLayout::from_index(id.index);
                self.clear_notice();
                None
            }
            ids::FONT => {
                self.cfg.ui_font = UiFont::from_index(id.index);
                self.clear_notice();
                None
            }
            ids::ACCENT => {
                if id.index < ACCENTS.len() {
                    self.cfg.ui_accent = id.index;
                    self.clear_notice();
                }
                None
            }
            ids::CHANNEL => {
                self.cfg.update_channel = Channel::from_index(id.index);
                self.clear_notice();
                None
            }
            ids::OCR => {
                self.cfg.ocr_backend = OcrBackend::from_index(id.index);
                self.clear_notice();
                None
            }
            ids::OCR_DIR => Some(Action::OpenOcrModels),
            ids::UPDATE => Some(Action::CheckUpdate),
            ids::EFFECTS => Some(Action::OpenEffects),
            ids::SCRIPTS => Some(Action::ChooseScript),
            ids::SCRIPTS_DIR => Some(Action::OpenScripts),
            ids::PHRASES => Some(Action::OpenPhrases),
            ids::PUNCT => Some(Action::OpenPunct),
            ids::APPS => Some(Action::OpenApps),
            ids::BACKUP_EXPORT => Some(Action::BackupExport),
            ids::BACKUP_IMPORT => Some(Action::BackupImport),
            ids::IMPORT_USER => Some(Action::ImportUser),
            ids::EXPORT_USER => Some(Action::ExportUser),
            ids::CLEAR_USER => Some(Action::ClearUser),
            ids::DIAGNOSTICS => Some(Action::Diagnostics),
            ids::RESET_FUZZY => Some(Action::ResetFuzzy),
            ids::CLOSE => Some(Action::Close),
            ids::SAVE => Some(Action::Save),
            _ => None,
        }
    }

    /// 左右键改分段组的值;返回有没有改成。
    pub fn shift(&mut self, id: &Id, delta: isize) -> bool {
        if !Self::is_segment(id.base) {
            return false;
        }
        let count = match id.base {
            ids::SCHEME => SchemeId::ALL.len(),
            ids::REGION => Region::NAMES.len(),
            ids::PAGE_SIZE => 3,
            ids::LAYOUT => UiLayout::NAMES.len(),
            ids::FONT => UiFont::NAMES.len(),
            ids::ACCENT => ACCENTS.len(),
            ids::CHANNEL => Channel::NAMES.len(),
            _ => return false,
        };
        let current = self.selected(id.base);
        let next = (current as isize + delta).rem_euclid(count as isize) as usize;
        self.activate(&Id::new(id.base, next));
        true
    }

    /// 这个前缀是不是分段组。
    pub fn is_segment(base: &str) -> bool {
        matches!(
            base,
            ids::SCHEME
                | ids::REGION
                | ids::PAGE_SIZE
                | ids::LAYOUT
                | ids::FONT
                | ids::ACCENT
                | ids::CHANNEL
        )
    }

    /// 分段组当前选中的是第几格。
    pub fn selected(&self, base: &str) -> usize {
        match base {
            ids::SCHEME => SchemeId::ALL
                .iter()
                .position(|id| *id == self.cfg.scheme)
                .unwrap_or(0),
            ids::REGION => self.cfg.region as usize,
            ids::PAGE_SIZE => [3usize, 5, 9]
                .iter()
                .position(|n| *n == self.cfg.page_size)
                .unwrap_or(1),
            ids::LAYOUT => usize::from(self.cfg.ui_layout == UiLayout::Vertical),
            ids::FONT => [UiFont::Small, UiFont::Standard, UiFont::Large]
                .iter()
                .position(|f| *f == self.cfg.ui_font)
                .unwrap_or(1),
            ids::ACCENT => self.cfg.ui_accent.min(ACCENTS.len() - 1),
            ids::CHANNEL => usize::from(self.cfg.update_channel == Channel::Beta),
            ids::OCR => usize::from(self.cfg.ocr_backend == OcrBackend::Ppocr),
            _ => 0,
        }
    }

    /// Tab 走一圈的次序:头部 → 卡片 → 页脚。
    pub fn focus_order() -> Vec<Id> {
        let mut order = vec![Id::new(ids::UPDATE, 0)];
        order.extend([
            Id::new(ids::SCHEME, 0),
            Id::new(ids::AUX, 0),
            Id::new(ids::CHINESE_PUNCT, 0),
            Id::new(ids::SHIFT_TOGGLE, 0),
            Id::new(ids::TRADITIONAL, 0),
            Id::new(ids::REGION, 0),
            Id::new(ids::PAGE_SIZE, 0),
            Id::new(ids::LAYOUT, 0),
            Id::new(ids::FONT, 0),
            Id::new(ids::ACCENT, 0),
            Id::new(ids::EFFECTS, 0),
            Id::new(ids::EFFECTS_TOGGLE, 0),
            Id::new(ids::SCRIPTS, 0),
            Id::new(ids::SCRIPTS_DIR, 0),
            Id::new(ids::OCR, 0),
            Id::new(ids::OCR_DIR, 0),
            Id::new(ids::PREDICT, 0),
            Id::new(ids::RESET_FUZZY, 0),
            Id::new(ids::FUZZY, 0),
            Id::new(ids::PHRASES, 0),
            Id::new(ids::PUNCT, 0),
            Id::new(ids::APPS, 0),
            Id::new(ids::BACKUP_EXPORT, 0),
            Id::new(ids::BACKUP_IMPORT, 0),
            Id::new(ids::IMPORT_USER, 0),
            Id::new(ids::EXPORT_USER, 0),
            Id::new(ids::CLEAR_USER, 0),
            Id::new(ids::DIAGNOSTICS, 0),
            Id::new(ids::CHANNEL, 0),
        ]);
        order.push(Id::new(ids::CLOSE, 0));
        order.push(Id::new(ids::SAVE, 0));
        order
    }
}

/// 「关于」里的状态行:词典来源与规模、模型是否加载。
pub fn status_detail(status: &Option<EngineStatus>) -> String {
    match status {
        Some(s) => format!(
            "词典 {}({}) · 语法模型{} · 重排 4M{} / 25M{}",
            word_count(s.dict_words),
            s.dict_source,
            mark(s.gram),
            mark(s.rerank4m),
            mark(s.rerank25m),
        ),
        None => "服务未运行,按任意键后再看".to_string(),
    }
}

/// 词条数的人类读法:上万用「万」,否则原样。
fn word_count(words: usize) -> String {
    if words >= 10_000 {
        let w = words as f32 / 10_000.0;
        if (w * 10.0).round() as i32 % 10 == 0 {
            format!("{w:.0} 万")
        } else {
            format!("{w:.1} 万")
        }
    } else {
        format!("{words} 词条")
    }
}

fn mark(on: bool) -> &'static str {
    if on { "✓" } else { "✗" }
}

#[cfg(test)]
#[path = "settings_view_tests.rs"]
mod tests;
