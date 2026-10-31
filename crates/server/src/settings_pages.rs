//! 设置页的静态构建:配置铺成分组、设置行与各栏详情文案。
//!
//! `settings_view` 的 `#[path]` 子模块(src 保持扁平):字段可见性
//! 不变,方法仍是 `Settings` 的 inherent impl,调用方路径不变。

use super::*;

use crate::config::OcrBackend;
use crate::settings_schema::{self, ControlDef, PageId, SettingDef};
use lueying_render::ui::{Button, ButtonKind, Control, Group, NavItem, Page, Row, SegmentOption};

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
        let active_nav = PageId::ALL
            .iter()
            .position(|candidate| *candidate == page)
            .unwrap_or(0);
        Page {
            title: "掠影".to_string(),
            subtitle: format!("拼音输入法 · v{}", self.current),
            header_button: Some(self.update_button()),
            nav,
            active_nav,
            groups: self.groups_page(page, colors),
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
            PageId::Candidate => vec![Group {
                title: PageId::Candidate.title().to_string(),
                rows: self.setting_rows(PageId::Candidate, colors),
            }],
            PageId::Extensions => {
                let mut rows = vec![
                    self.script_row(),
                    self.consent_row(),
                    self.audit_row(),
                    self.ext_metrics_row(),
                ];
                rows.extend(self.setting_rows(PageId::Extensions, colors));
                rows.push(self.ocr_dir_row());
                let mut groups = vec![Group {
                    title: PageId::Extensions.title().to_string(),
                    rows,
                }];
                // 声明式扩展设置紧跟在「扩展」分组后面。
                groups.extend(self.ext_setting_groups());
                groups
            }
            PageId::Dictionary => vec![Group {
                title: PageId::Dictionary.title().to_string(),
                rows: vec![
                    self.phrases_row(),
                    self.punct_row(),
                    self.apps_row(),
                    self.backup_row(),
                    self.user_dict_row(),
                ],
            }],
            PageId::About => vec![Group {
                title: PageId::About.title().to_string(),
                rows: vec![
                    Row {
                        label: "引擎状态".to_string(),
                        detail: status_detail(&self.status),
                        control: Control::None,
                    },
                    self.setting_row(
                        settings_schema::find(ids::CHANNEL).expect("更新渠道在 schema 里"),
                        colors,
                    ),
                    self.diagnostics_row(),
                ],
            }],
        }
    }

    /// 一页里由 schema 生成的简单设置行。
    fn setting_rows(&self, page: PageId, colors: &PageColors) -> Vec<Row> {
        settings_schema::on_page(page)
            .filter(|def| {
                def.ui
                    .as_ref()
                    .is_none_or(|ui| (ui.visible)(&self.cfg))
            })
            .map(|def| self.setting_row(def, colors))
            .collect()
    }

    /// 把一条 schema 描述变成一行控件。
    fn setting_row(&self, def: &SettingDef, colors: &PageColors) -> Row {
        let ui = def.ui.as_ref().expect("setting_row 只接受有界面的设置");
        let control = match &ui.control {
            ControlDef::Toggle { id, get, .. } => {
                let control_id = Id::new(*id, 0);
                let on = get(&self.cfg);
                match def.key {
                    "effects" => Control::ButtonsToggle {
                        items: vec![Button {
                            id: Id::new(ids::EFFECTS, 0),
                            text: "目录".to_string(),
                            kind: ButtonKind::Secondary,
                            enabled: true,
                        }],
                        toggle: control_id,
                        toggle_on: on,
                    },
                    "fuzzy" => Control::ButtonsToggle {
                        items: vec![Button {
                            id: Id::new(ids::RESET_FUZZY, 0),
                            text: "重置".to_string(),
                            kind: ButtonKind::Ghost,
                            enabled: self.reset_enabled(),
                        }],
                        toggle: control_id,
                        toggle_on: on,
                    },
                    _ => Control::Toggle {
                        id: control_id,
                        on,
                    },
                }
            }
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
                "fuzzy" => self.fuzzy_detail(),
                "ocr_engine" => self.ocr_detail(),
                _ => def.detail.to_string(),
            },
            control,
        }
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
            "运行期脚本",
            self.script_detail(),
            vec![
                Self::secondary(ids::SCRIPTS, "选择", true),
                Self::secondary(ids::SCRIPTS_DIR, "目录", true),
            ],
        )
    }

    fn consent_row(&self) -> Row {
        self.action_row(
            "扩展授权",
            crate::settings_consent::detail(&self.cfg.extensions, &self.scripts),
            vec![Button {
                id: Id::new(ids::CONSENT, 0),
                text: "同意".to_string(),
                kind: ButtonKind::Primary,
                enabled: crate::settings_consent::approve_enabled(
                    &self.cfg.extensions,
                    &self.scripts,
                ),
            }],
        )
    }

    fn audit_row(&self) -> Row {
        self.action_row(
            "审计日志",
            "谁在什么时候调用了截图、识别与上屏".to_string(),
            vec![Self::secondary(ids::AUDIT, "查看", true)],
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
            "放入 det.onnx、rec.onnx、dict.txt".to_string(),
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

    fn apps_row(&self) -> Row {
        self.action_row(
            "按应用配置",
            "在 apps.toml 里按应用关标点或学习".to_string(),
            vec![Self::secondary(ids::APPS, "编辑", true)],
        )
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
            "导入、导出或清空本机学习记录".to_string(),
            vec![
                Self::secondary(ids::IMPORT_USER, "导入", true),
                Self::secondary(ids::EXPORT_USER, "导出", true),
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
        crate::settings_ext::groups(&self.cfg.extensions, &self.scripts, &self.cfg)
    }

    /// 当前页面上实际渲染的扩展字段。
    pub(crate) fn ext_fields(&self) -> Vec<crate::settings_ext::Field> {
        crate::settings_ext::fields(&self.cfg.extensions, &self.scripts, &self.cfg)
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
    pub(crate) fn script_detail(&self) -> String {
        match self.cfg.extensions.as_slice() {
            [] => {
                if self.scripts.is_empty() {
                    "未安装扩展;点「目录」放入扩展包".to_string()
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

    /// 扩展运行那行的说明:状态计数、步进峰值与超限次数。
    pub(crate) fn ext_metrics_detail(&self) -> String {
        match &self.ext_metrics {
            Some(metrics) => metrics.summary(),
            None => "暂无数据;服务启动后写入".to_string(),
        }
    }

    /// 截图识别那行的说明:选的引擎与模型是否可用。
    pub(crate) fn ocr_detail(&self) -> String {
        match (self.cfg.ocr_backend, self.ppocr_ready) {
            (OcrBackend::Ppocr, true) => "随包 PP-OCRv6 · 首次识别加载".to_string(),
            (OcrBackend::Ppocr, false) => "模型未安装;点「目录」放入三个文件".to_string(),
            (OcrBackend::System, true) => "系统 OCR;也可切到 PP-OCRv6".to_string(),
            (OcrBackend::System, false) => "系统 OCR;PP-OCRv6 模型未安装".to_string(),
        }
    }
}
