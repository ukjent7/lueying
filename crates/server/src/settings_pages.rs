//! 设置页的静态构建:配置铺成分组、设置行与各栏详情文案。
//!
//! `settings_view` 的 `#[path]` 子模块(src 保持扁平):字段可见性
//! 不变,方法仍是 `Settings` 的 inherent impl,调用方路径不变。

use super::*;

impl Settings {
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

    /// 固定分组 + 扩展声明的设置分组。
    fn groups_page(&self, colors: &PageColors) -> Vec<Group> {
        let mut groups = vec![
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
                        label: "扩展授权".to_string(),
                        detail: crate::settings_consent::detail(
                            &self.cfg.extensions,
                            &self.scripts,
                        ),
                        control: Control::Buttons {
                            items: vec![Button {
                                id: Id::new(ids::CONSENT, 0),
                                text: "同意".to_string(),
                                kind: ButtonKind::Primary,
                                enabled: crate::settings_consent::approve_enabled(
                                    &self.cfg.extensions,
                                    &self.scripts,
                                ),
                            }],
                        },
                    },
                    Row {
                        label: "审计日志".to_string(),
                        detail: "谁在什么时候调用了截图、识别与上屏".to_string(),
                        control: Control::Buttons {
                            items: vec![Button {
                                id: Id::new(ids::AUDIT, 0),
                                text: "查看".to_string(),
                                kind: ButtonKind::Secondary,
                                enabled: true,
                            }],
                        },
                    },
                    Row {
                        label: "扩展运行".to_string(),
                        detail: self.ext_metrics_detail(),
                        control: Control::Buttons {
                            items: vec![Button {
                                id: Id::new(ids::EXT_METRICS, 0),
                                text: "刷新".to_string(),
                                kind: ButtonKind::Secondary,
                                enabled: true,
                            }],
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
                        detail: "在设置窗里编辑 phrases.txt，保存后重载".to_string(),
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
                        detail: "在 apps.toml 里按应用关标点或学习".to_string(),
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
        ];
        // 声明式扩展设置紧跟在「扩展」分组后面。
        groups.splice(3..3, self.ext_setting_groups());
        groups
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
    fn ext_metrics_detail(&self) -> String {
        match &self.ext_metrics {
            Some(metrics) => metrics.summary(),
            None => "暂无数据;服务启动后写入".to_string(),
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
}
