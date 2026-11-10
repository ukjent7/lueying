//! 设置页的静态构建:配置铺成分组、设置行与各栏详情文案。
//!
//! `settings::view` 的子模块:字段可见性
//! 不变,方法仍是 `Settings` 的 inherent impl,调用方路径不变。

use super::*;

use crate::settings::model::{
    Button, ButtonKind, CandidatePreview, Control, Group, NavItem, Page, Row, SegmentOption,
};
use crate::settings::schema::{self, ControlDef, PageId, SettingDef};
use lueying_server::config::OcrBackend;

impl Settings {
    /// 当前页面:左栏导航 + 右栏设置卡片。
    pub fn page(&self, colors: &PageColors) -> Page {
        self.page_view(self.page, colors)
    }

    /// 指定页面渲染;E2E 与无障碍树用它检查非当前页。
    pub fn page_view(&self, page: PageId, colors: &PageColors) -> Page {
        let nav = PageId::ALL
            .iter()
            .map(|page| NavItem {
                id: Id::new(page.nav_id(), 0),
                text: page.title().to_string(),
            })
            .collect();
        let searching = self.searching();
        let active_nav = if searching {
            usize::MAX
        } else {
            PageId::ALL
                .iter()
                .position(|candidate| *candidate == page)
                .unwrap_or(0)
        };
        let groups = if searching {
            self.search_groups(colors)
        } else {
            self.groups_page(page, colors)
        };
        let matched: usize = groups.iter().map(|group| group.rows.len()).sum();
        let subtitle = if searching {
            format!("搜索「{}」· {} 项", self.search.trim(), matched)
        } else {
            format!("拼音输入法 · v{}", self.current)
        };
        Page {
            title: if searching { "搜索" } else { "掠影" }.to_string(),
            subtitle,
            search: self.search.clone(),
            header_button: Some(self.update_button()),
            nav,
            active_nav,
            groups,
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

    /// 某一页的分组。
    fn groups_page(&self, page: PageId, colors: &PageColors) -> Vec<Group> {
        match page {
            PageId::Input => vec![Group {
                title: PageId::Input.title().to_string(),
                rows: self.setting_rows(PageId::Input, colors),
            }],
            PageId::Candidate => {
                let mut rows = vec![self.candidate_preview_row(colors)];
                rows.extend(self.setting_rows(PageId::Candidate, colors));
                vec![Group {
                    title: PageId::Candidate.title().to_string(),
                    rows,
                }]
            }
            PageId::Extensions => {
                let mut groups = Vec::new();
                if !self.scripts.is_empty() {
                    groups.push(Group {
                        title: "已安装扩展".to_string(),
                        rows: self.installed_extension_rows(),
                    });
                }
                let mut rows = vec![
                    self.script_row(),
                    self.consent_row(),
                    self.ext_metrics_row(),
                ];
                rows.extend(self.setting_rows(PageId::Extensions, colors));
                rows.push(self.ocr_dir_row());
                groups.push(Group {
                    title: PageId::Extensions.title().to_string(),
                    rows,
                });
                // 声明式扩展设置紧跟在「扩展」分组后面。
                groups.extend(self.ext_setting_groups());
                groups
            }
            PageId::Dictionary => vec![
                Group {
                    title: PageId::Dictionary.title().to_string(),
                    rows: vec![
                        self.phrases_row(),
                        self.punct_row(),
                        self.backup_row(),
                        self.user_dict_row(),
                    ],
                },
                Group {
                    title: "按应用配置".to_string(),
                    rows: self.apps_rows(),
                },
            ],
            PageId::About => vec![Group {
                title: PageId::About.title().to_string(),
                rows: vec![
                    Row {
                        label: "引擎状态".to_string(),
                        detail: status_detail(&self.status),
                        control: Control::None,
                    },
                    self.setting_row(
                        schema::find(ids::CHANNEL).expect("更新渠道在 schema 里"),
                        colors,
                    ),
                    self.diagnostics_row(),
                ],
            }],
        }
    }

    /// 跨页搜索结果:保留原分组标题,只留标题或说明命中关键词的行。
    fn search_groups(&self, colors: &PageColors) -> Vec<Group> {
        let needle = self.search.trim().to_lowercase();
        let mut out = Vec::new();
        for page in PageId::ALL {
            let mut groups = self.groups_page(page, colors);
            for group in &mut groups {
                group.rows.retain(|row| {
                    row.label.to_lowercase().contains(&needle)
                        || row.detail.to_lowercase().contains(&needle)
                });
            }
            groups.retain(|group| !group.rows.is_empty());
            out.extend(groups);
        }
        out
    }

    /// 一页里由 schema 生成的简单设置行。
    fn setting_rows(&self, page: PageId, colors: &PageColors) -> Vec<Row> {
        schema::on_page(page)
            .filter(|def| def.ui.as_ref().is_none_or(|ui| (ui.visible)(&self.cfg)))
            .map(|def| self.setting_row(def, colors))
            .collect()
    }

    /// 候选窗实时预览:同一份配置直接渲染样例候选。
    fn candidate_preview_row(&self, colors: &PageColors) -> Row {
        let accent = colors.accents.get(self.cfg.ui_accent).copied().unwrap_or(0);
        Row {
            label: "候选窗预览".to_string(),
            detail: "改字号、排列或主题色会实时反映".to_string(),
            control: Control::Preview {
                id: Id::new("candidate-preview", 0),
                style: CandidatePreview {
                    accent,
                    font_scale: self.cfg.ui_font.factor(),
                    vertical: self.cfg.ui_layout == lueying_server::config::UiLayout::Vertical,
                    page_size: self.cfg.page_size,
                    pinyin: "lue ying".to_string(),
                    candidates: vec![
                        "掠影".to_string(),
                        "略影".to_string(),
                        "掠应".to_string(),
                        "月影".to_string(),
                        "掠映".to_string(),
                        "略赢".to_string(),
                        "掠鹰".to_string(),
                        "略应".to_string(),
                        "掠缨".to_string(),
                    ],
                },
            },
        }
    }

    /// 把一条 schema 描述变成一行控件。
    fn setting_row(&self, def: &SettingDef, colors: &PageColors) -> Row {
        let ui = def.ui.as_ref().expect("setting_row 只接受有界面的设置");
        let control = match &ui.control {
            ControlDef::Toggle { id, get, .. } => Control::Toggle {
                id: Id::new(*id, 0),
                on: get(&self.cfg),
            },
            ControlDef::Segment {
                id,
                choices,
                width,
                tinted,
                get,
                ..
            } => Control::Segment {
                id: Id::new(*id, 0),
                options: choices
                    .iter()
                    .enumerate()
                    .map(|(index, choice)| {
                        if *tinted {
                            let tint = colors.accents.get(index).copied().unwrap_or(0);
                            SegmentOption::tinted(choice.label, tint)
                        } else {
                            SegmentOption::new(choice.label)
                        }
                    })
                    .collect(),
                selected: get(&self.cfg),
                width: *width,
            },
        };
        Row {
            label: def.title.to_string(),
            detail: match def.key {
                "ocr_engine" => self.ocr_detail(),
                _ => def.detail.to_string(),
            },
            control,
        }
    }

    /// 已安装扩展:一行一个开关;权限状态写在说明里。
    fn installed_extension_rows(&self) -> Vec<Row> {
        self.scripts
            .iter()
            .take(ids::SCRIPT_SLOTS)
            .enumerate()
            .map(|(slot, script)| {
                let enabled = self.cfg.extensions.iter().any(|id| id == &script.id);
                let consent = match &script.consent {
                    ConsentState::Approved => "已授权",
                    ConsentState::Missing => "待同意",
                    ConsentState::Changed { .. } => "需重新同意",
                };
                Row {
                    label: script.name.clone(),
                    detail: format!(
                        "{}{} · {} · {consent}",
                        if script.native {
                            "原生扩展 · "
                        } else {
                            ""
                        },
                        script.id,
                        if enabled { "已启用" } else { "未启用" }
                    ),
                    control: Control::ButtonsToggle {
                        items: Vec::new(),
                        toggle: Id::new(ids::script_toggle(slot), 0),
                        toggle_on: enabled,
                    },
                }
            })
            .collect()
    }

    fn action_row(&self, label: &str, detail: String, buttons: Vec<Button>) -> Row {
        Row {
            label: label.to_string(),
            detail,
            control: Control::Buttons { items: buttons },
        }
    }

    fn secondary(id: &'static str, text: &str, enabled: bool) -> Button {
        Button {
            id: Id::new(id, 0),
            text: text.to_string(),
            kind: ButtonKind::Secondary,
            enabled,
        }
    }

    fn script_row(&self) -> Row {
        self.action_row(
            "运行期扩展",
            self.script_detail(),
            vec![
                Self::secondary(ids::SCRIPTS, "选择", true),
                Self::secondary(ids::IMPORT_LUEXT, "导入", true),
                Self::secondary(ids::SCRIPTS_DIR, "目录", true),
            ],
        )
    }

    fn consent_row(&self) -> Row {
        self.action_row(
            "扩展授权",
            crate::settings::consent::detail(&self.cfg.extensions, &self.scripts),
            vec![Button {
                id: Id::new(ids::CONSENT, 0),
                text: "同意".to_string(),
                kind: ButtonKind::Primary,
                enabled: crate::settings::consent::approve_enabled(
                    &self.cfg.extensions,
                    &self.scripts,
                ),
            }],
        )
    }

    fn ext_metrics_row(&self) -> Row {
        self.action_row(
            "扩展运行",
            self.ext_metrics_detail(),
            vec![Self::secondary(ids::EXT_METRICS, "刷新", true)],
        )
    }

    fn ocr_dir_row(&self) -> Row {
        self.action_row(
            "识别模型",
            "PP-OCRv6 模型缺失时可用系统 OCR".to_string(),
            vec![Self::secondary(ids::OCR_DIR, "目录", true)],
        )
    }

    fn phrases_row(&self) -> Row {
        self.action_row(
            "自定义短语",
            "在设置窗里编辑 phrases.txt，保存后重载".to_string(),
            vec![Self::secondary(ids::PHRASES, "编辑", true)],
        )
    }

    fn punct_row(&self) -> Row {
        self.action_row(
            "自定义标点",
            "编辑 punct.txt,映射半角符号的输出".to_string(),
            vec![Self::secondary(ids::PUNCT, "编辑", true)],
        )
    }

    /// 按应用配置:一行一个宿主程序,标点与学习各一个三态按钮。
    fn apps_rows(&self) -> Vec<Row> {
        let mut rows = vec![self.action_row(
            "添加应用",
            "从正在运行的程序里选择;添加后保存生效".to_string(),
            vec![Self::secondary(ids::APPS_ADD, "选择", true)],
        )];
        for (slot, (host, over)) in self.cfg.apps.iter().take(ids::APP_SLOTS).enumerate() {
            let punct = override_label(over.punct);
            let learning = override_label(over.learning);
            rows.push(Row {
                label: host.clone(),
                detail: format!("中文标点 {punct} · 学习 {learning}"),
                control: Control::Buttons {
                    items: vec![
                        Button {
                            id: Id::new(ids::app_punct(slot), 0),
                            text: format!("标点:{punct}"),
                            kind: ButtonKind::Secondary,
                            enabled: true,
                        },
                        Button {
                            id: Id::new(ids::app_learning(slot), 0),
                            text: format!("学习:{learning}"),
                            kind: ButtonKind::Secondary,
                            enabled: true,
                        },
                    ],
                },
            });
        }
        rows.push(self.action_row(
            "配置文件",
            "图形列表之外的读写仍可直接编辑 apps.toml".to_string(),
            vec![Self::secondary(ids::APPS, "打开", true)],
        ));
        rows
    }

    fn backup_row(&self) -> Row {
        self.action_row(
            "配置备份",
            "配置、短语、标点、按应用与词库打包成一份文本".to_string(),
            vec![
                Self::secondary(ids::BACKUP_EXPORT, "导出", true),
                Self::secondary(ids::BACKUP_IMPORT, "导入", true),
            ],
        )
    }

    fn user_dict_row(&self) -> Row {
        self.action_row(
            "用户词库",
            "导入标准词表(词/拼音/权重)或本机导出的词库;导出本机词库".to_string(),
            vec![
                Self::secondary(ids::IMPORT_USER, "导入", true),
                Self::secondary(ids::EXPORT_USER, "导出", true),
            ],
        )
    }

    fn diagnostics_row(&self) -> Row {
        self.action_row(
            "诊断信息",
            "版本、配置与服务日志导出成一份文本".to_string(),
            vec![Self::secondary(ids::DIAGNOSTICS, "导出", true)],
        )
    }

    /// 扩展声明式设置展开成的分组。
    fn ext_setting_groups(&self) -> Vec<Group> {
        crate::settings::ext::groups(&self.cfg.extensions, &self.scripts, &self.cfg)
    }

    /// 当前页面上实际渲染的扩展字段。
    pub(crate) fn ext_fields(&self) -> Vec<crate::settings::ext::Field> {
        crate::settings::ext::fields(&self.cfg.extensions, &self.scripts, &self.cfg)
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

    /// 运行期扩展那行的说明:当前包,或者怎么开始。
    pub(crate) fn script_detail(&self) -> String {
        match self.cfg.extensions.as_slice() {
            [] => {
                if self.scripts.is_empty() {
                    "未安装扩展;点「导入」装 .luext 包,或把包目录放进扩展目录".to_string()
                } else {
                    format!("未启用 · 已安装 {} 个", self.scripts.len())
                }
            }
            [id] => match self.scripts.iter().find(|script| &script.id == id) {
                Some(script) => {
                    let pending = if script.consent == ConsentState::Approved {
                        ""
                    } else {
                        " · 待同意"
                    };
                    format!("{} · {}{pending}", script.name, script.id)
                }
                None => format!("{id} · 未安装"),
            },
            ids => format!("已启用 {} 个 · {}", ids.len(), ids.join(", ")),
        }
    }

    /// 扩展运行那行的说明:状态计数与派发峰值。
    pub(crate) fn ext_metrics_detail(&self) -> String {
        match &self.ext_metrics {
            Some(metrics) => metrics.summary(),
            None => "暂无数据;服务启动后写入".to_string(),
        }
    }

    /// 截图识别那行的说明:选的引擎与扩展是否可用。
    pub(crate) fn ocr_detail(&self) -> String {
        match (self.cfg.ocr_backend, self.ppocr_ready) {
            (OcrBackend::Ppocr, true) => {
                "PP-OCRv6 · 原生扩展,首次识别拉起进程,识别不联网".to_string()
            }
            (OcrBackend::Ppocr, false) => "识别引擎未安装;用 lueying ext add 安装".to_string(),
            (OcrBackend::System, true) => "系统 OCR;也可切到 PP-OCRv6".to_string(),
            (OcrBackend::System, false) => "系统 OCR;PP-OCRv6 引擎未安装,可按需安装".to_string(),
        }
    }
}

/// 按应用覆盖项的三态文字。
fn override_label(value: Option<bool>) -> &'static str {
    match value {
        None => "跟随",
        Some(true) => "开",
        Some(false) => "关",
    }
}
