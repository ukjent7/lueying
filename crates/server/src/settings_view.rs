//! 设置窗的页面模型:配置铺成一页,点控件改配置。
//!
//! 这一层不碰 Windows:页面长什么样、点一下改哪个配置、Tab 走到哪,都在
//! 这里;平台侧只管窗口、绘制与文件对话框。Linux E2E 因此能直接断言
//! 设置页的结构与交互结果。

use lueying_engine::Fuzzy;
use lueying_extmeta::metrics::ExtMetrics;
use lueying_features::SchemeId;
use lueying_protocol::EngineStatus;
use lueying_render::ui::Id;

use crate::accents::ACCENTS;
use crate::config::{AppConfig, Channel, OcrBackend, Region, UiFont, UiLayout};

pub use crate::settings_consent::{ConsentState, ScriptEntry};

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
    pub const CONSENT: &str = "consent";
    pub const AUDIT: &str = "audit";
    pub const EXT_METRICS: &str = "ext-metrics";
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
    /// 扩展声明式设置最多渲染多少个字段。
    pub const EXT_FIELDS: usize = 16;

    /// 第 `slot` 个扩展设置字段的控件前缀。
    pub fn ext_field(slot: usize) -> &'static str {
        match slot {
            0 => "ext-0",
            1 => "ext-1",
            2 => "ext-2",
            3 => "ext-3",
            4 => "ext-4",
            5 => "ext-5",
            6 => "ext-6",
            7 => "ext-7",
            8 => "ext-8",
            9 => "ext-9",
            10 => "ext-10",
            11 => "ext-11",
            12 => "ext-12",
            13 => "ext-13",
            14 => "ext-14",
            15 => "ext-15",
            _ => "ext-0",
        }
    }
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
    ApproveExtension,
    OpenExtAudit,
    RefreshExtMetrics,
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
    /// 服务端写下的扩展运行指标;服务未运行时是 None。
    pub ext_metrics: Option<ExtMetrics>,
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
            ext_metrics: None,
        }
    }
}

#[path = "settings_pages.rs"]
mod pages;

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

    /// 当前选中的包;没启用或启用了多个返回 None。
    pub fn selected_script(&self) -> Option<&ScriptEntry> {
        crate::settings_consent::selected(&self.cfg.extensions, &self.scripts)
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
        if let Some(slot) = crate::settings_ext::slot(id.base) {
            if let Some(field) = self
                .ext_fields()
                .into_iter()
                .find(|field| field.slot == slot)
                && crate::settings_ext::apply(&mut self.cfg, &field, id.index)
            {
                self.clear_notice();
            }
            return None;
        }
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
            ids::CONSENT => Some(Action::ApproveExtension),
            ids::AUDIT => Some(Action::OpenExtAudit),
            ids::EXT_METRICS => Some(Action::RefreshExtMetrics),
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
        if let Some(slot) = crate::settings_ext::slot(id.base) {
            let Some(field) = self
                .ext_fields()
                .into_iter()
                .find(|field| field.slot == slot)
            else {
                return false;
            };
            let crate::settings_ext::Kind::Choice { options, selected } = &field.kind else {
                return false;
            };
            if options.is_empty() {
                return false;
            }
            let next = (*selected as isize + delta).rem_euclid(options.len() as isize) as usize;
            crate::settings_ext::apply(&mut self.cfg, &field, next);
            self.clear_notice();
            return true;
        }
        let count = match id.base {
            ids::SCHEME => SchemeId::ALL.len(),
            ids::REGION => Region::NAMES.len(),
            ids::PAGE_SIZE => 3,
            ids::LAYOUT => UiLayout::NAMES.len(),
            ids::FONT => UiFont::NAMES.len(),
            ids::ACCENT => ACCENTS.len(),
            ids::CHANNEL => Channel::NAMES.len(),
            ids::OCR => 2,
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
                | ids::OCR
        ) || crate::settings_ext::slot(base).is_some()
    }

    /// 分段组当前选中的是第几格。
    pub fn selected(&self, base: &str) -> usize {
        if let Some(slot) = crate::settings_ext::slot(base) {
            return self
                .ext_fields()
                .into_iter()
                .find(|field| field.slot == slot)
                .map_or(0, |field| field.selected());
        }
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
}

pub use crate::settings_text::status_detail;

#[cfg(test)]
#[path = "settings_view_tests.rs"]
mod tests;
