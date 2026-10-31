//! 设置窗的页面模型:左栏分类导航,右栏设置卡片,点控件改配置。
//!
//! 这一层不碰 Windows:页面长什么样、点一下改哪个配置、Tab 走到哪,都在
//! 这里;平台侧只管窗口、绘制与文件对话框。Linux E2E 因此能直接断言
//! 设置页的结构与交互结果。

use lueying_extmeta::metrics::ExtMetrics;
use lueying_protocol::EngineStatus;
use lueying_render::ui::Id;

use crate::config::AppConfig;
use crate::settings_schema::ControlDef;

pub use crate::settings_schema::PageId;

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
    pub const APPS_ADD: &str = "apps-add";
    /// 按应用列表最多渲染多少行。
    pub const APP_SLOTS: usize = 16;
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
    /// 官方扩展目录最多渲染多少项。
    pub const OFFICIAL_SLOTS: usize = 8;
    /// 已安装扩展列表最多渲染多少项。
    pub const SCRIPT_SLOTS: usize = 16;

    /// 第 `slot` 个官方扩展的安装按钮前缀。
    pub fn official_ext(slot: usize) -> &'static str {
        match slot {
            0 => "official-0",
            1 => "official-1",
            2 => "official-2",
            3 => "official-3",
            4 => "official-4",
            5 => "official-5",
            6 => "official-6",
            7 => "official-7",
            _ => "official-0",
        }
    }

    /// 安装按钮前缀 → 目录下标。
    pub fn official_slot(base: &str) -> Option<usize> {
        let slot: usize = base.strip_prefix("official-")?.parse().ok()?;
        (slot < OFFICIAL_SLOTS).then_some(slot)
    }

    /// 第 `slot` 个已安装扩展的启用开关前缀。
    pub fn script_toggle(slot: usize) -> &'static str {
        match slot {
            0 => "script-0",
            1 => "script-1",
            2 => "script-2",
            3 => "script-3",
            4 => "script-4",
            5 => "script-5",
            6 => "script-6",
            7 => "script-7",
            8 => "script-8",
            9 => "script-9",
            10 => "script-10",
            11 => "script-11",
            12 => "script-12",
            13 => "script-13",
            14 => "script-14",
            15 => "script-15",
            _ => "script-0",
        }
    }

    /// 启用开关前缀 → 已安装列表下标。
    pub fn script_slot(base: &str) -> Option<usize> {
        let slot: usize = base.strip_prefix("script-")?.parse().ok()?;
        (slot < SCRIPT_SLOTS).then_some(slot)
    }

    /// 按应用列表第 `slot` 行的标点按钮前缀。
    pub fn app_punct(slot: usize) -> &'static str {
        const IDS: [&str; APP_SLOTS] = [
            "app-punct-0",
            "app-punct-1",
            "app-punct-2",
            "app-punct-3",
            "app-punct-4",
            "app-punct-5",
            "app-punct-6",
            "app-punct-7",
            "app-punct-8",
            "app-punct-9",
            "app-punct-10",
            "app-punct-11",
            "app-punct-12",
            "app-punct-13",
            "app-punct-14",
            "app-punct-15",
        ];
        IDS.get(slot).copied().unwrap_or(IDS[0])
    }

    /// 标点按钮前缀 → 按应用列表下标。
    pub fn app_punct_slot(base: &str) -> Option<usize> {
        let slot: usize = base.strip_prefix("app-punct-")?.parse().ok()?;
        (slot < APP_SLOTS).then_some(slot)
    }

    /// 按应用列表第 `slot` 行的学习按钮前缀。
    pub fn app_learning(slot: usize) -> &'static str {
        const IDS: [&str; APP_SLOTS] = [
            "app-learning-0",
            "app-learning-1",
            "app-learning-2",
            "app-learning-3",
            "app-learning-4",
            "app-learning-5",
            "app-learning-6",
            "app-learning-7",
            "app-learning-8",
            "app-learning-9",
            "app-learning-10",
            "app-learning-11",
            "app-learning-12",
            "app-learning-13",
            "app-learning-14",
            "app-learning-15",
        ];
        IDS.get(slot).copied().unwrap_or(IDS[0])
    }

    /// 学习按钮前缀 → 按应用列表下标。
    pub fn app_learning_slot(base: &str) -> Option<usize> {
        let slot: usize = base.strip_prefix("app-learning-")?.parse().ok()?;
        (slot < APP_SLOTS).then_some(slot)
    }

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

/// 按应用开关的三态循环:跟随全局 → 开 → 关 → 跟随全局。
fn cycle_override(current: Option<bool>) -> Option<bool> {
    match current {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    }
}

/// 页面里要用的颜色,由平台侧按主题算好传进来。
#[derive(Clone, Debug, Default)]
pub struct PageColors {
    /// 普通提示文字的颜色。
    pub notice_info: u32,
    /// 成功提示的颜色。
    pub notice_success: u32,
    /// 失败提示的颜色。
    pub notice_error: u32,
    /// 主题色分段里每一格的颜色,顺序同 `crate::accents::ACCENTS`。
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
    /// 安装官方目录里的扩展或模型;id 来自目录项。
    InstallOfficial(String),
    /// 编辑扩展密钥:扩展 id、设置键、显示标题。
    EditSecret(String, String, String),
    OpenPhrases,
    OpenPunct,
    OpenApps,
    /// 从正在运行的程序里挑一个加入按应用配置。
    AddApp,
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
    /// 左栏当前打开的页面。
    pub page: PageId,
    /// 顶部搜索框的文字;非空时右栏显示跨页搜索结果。
    pub search: String,
    /// 正在安装的官方扩展/模型 id;下载在后台线程进行。
    pub installing: Option<String>,
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
            page: PageId::Input,
            search: String::new(),
            installing: None,
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
        if let Some(page) = PageId::from_nav(id.base) {
            self.page = page;
            self.search.clear();
            self.clear_notice();
            return None;
        }
        if let Some(slot) = ids::official_slot(id.base) {
            let Some(extension) = crate::official_extensions::catalog().extensions.get(slot) else {
                return None;
            };
            let installed = if extension.is_model() {
                extension.installed_model()
            } else {
                self.scripts.iter().any(|script| script.id == extension.id)
            };
            if installed || self.installing.is_some() {
                return None;
            }
            self.clear_notice();
            return Some(Action::InstallOfficial(extension.id.clone()));
        }
        if let Some(slot) = ids::script_slot(id.base) {
            let Some(script) = self.scripts.get(slot) else {
                return None;
            };
            let script_id = script.id.clone();
            if let Some(index) = self
                .cfg
                .extensions
                .iter()
                .position(|candidate| candidate == &script_id)
            {
                self.cfg.extensions.remove(index);
            } else {
                self.cfg.extensions.push(script_id);
            }
            self.clear_notice();
            return None;
        }
        if let Some(slot) = ids::app_punct_slot(id.base) {
            if let Some(host) = self.cfg.apps.keys().nth(slot).cloned() {
                let current = self.cfg.apps[&host].punct;
                if let Some(over) = self.cfg.apps.get_mut(&host) {
                    over.punct = cycle_override(current);
                }
                self.clear_notice();
            }
            return None;
        }
        if let Some(slot) = ids::app_learning_slot(id.base) {
            if let Some(host) = self.cfg.apps.keys().nth(slot).cloned() {
                let current = self.cfg.apps[&host].learning;
                if let Some(over) = self.cfg.apps.get_mut(&host) {
                    over.learning = cycle_override(current);
                }
                self.clear_notice();
            }
            return None;
        }
        if let Some(def) = crate::settings_schema::find(id.base) {
            let Some(ui) = &def.ui else {
                return None;
            };
            match &ui.control {
                ControlDef::Toggle { get, set, .. } => {
                    let next = !get(&self.cfg);
                    set(&mut self.cfg, next);
                }
                ControlDef::Segment { choices, set, .. } => {
                    if id.index < choices.len() {
                        set(&mut self.cfg, id.index);
                    }
                }
            }
            self.clear_notice();
            return None;
        }
        if let Some(slot) = crate::settings_ext::slot(id.base) {
            let Some(field) = self
                .ext_fields()
                .into_iter()
                .find(|field| field.slot == slot)
            else {
                return None;
            };
            if field.is_secret() {
                return Some(Action::EditSecret(
                    field.ext.clone(),
                    field.key.clone(),
                    field.title.clone(),
                ));
            }
            if crate::settings_ext::apply(&mut self.cfg, &field, id.index) {
                self.clear_notice();
            }
            return None;
        }
        match id.base {
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
            ids::APPS_ADD => Some(Action::AddApp),
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
        if let Some(def) = crate::settings_schema::find(id.base) {
            let Some(ui) = &def.ui else {
                return false;
            };
            let ControlDef::Segment { choices, .. } = &ui.control else {
                return false;
            };
            if choices.is_empty() {
                return false;
            }
            let current = self.selected(id.base);
            let next = (current as isize + delta).rem_euclid(choices.len() as isize) as usize;
            self.activate(&Id::new(id.base, next));
            return true;
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
        false
    }

    /// 这个前缀是不是分段组。
    pub fn is_segment(base: &str) -> bool {
        crate::settings_schema::find(base).is_some_and(|def| {
            def.ui
                .as_ref()
                .is_some_and(|ui| matches!(&ui.control, ControlDef::Segment { .. }))
        }) || crate::settings_ext::slot(base).is_some()
    }

    /// 分段组当前选中的是第几格。
    pub fn selected(&self, base: &str) -> usize {
        if let Some(ui) = crate::settings_schema::find(base).and_then(|def| def.ui.as_ref()) {
            match &ui.control {
                ControlDef::Toggle { get, .. } => return usize::from(get(&self.cfg)),
                ControlDef::Segment { get, .. } => return get(&self.cfg),
            }
        }
        if let Some(slot) = crate::settings_ext::slot(base) {
            return self
                .ext_fields()
                .into_iter()
                .find(|field| field.slot == slot)
                .map_or(0, |field| field.selected());
        }
        0
    }

    /// 切到左栏的某一页;搜索状态随之清空。
    pub fn set_page(&mut self, page: PageId) {
        self.page = page;
        self.search.clear();
    }

    /// 更新顶部搜索词;空串恢复当前页。
    pub fn set_search(&mut self, text: impl Into<String>) {
        self.search = text.into();
        self.clear_notice();
    }

    /// 搜索是否处于激活状态。
    pub fn searching(&self) -> bool {
        !self.search.trim().is_empty()
    }
}

pub use crate::settings_text::status_detail;

#[cfg(test)]
#[path = "settings_view_tests.rs"]
mod tests;
