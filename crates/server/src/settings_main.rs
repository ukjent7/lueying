//! 设置窗独立进程:候选数、中文标点、简繁输出、Shift 切中英。
//! 模糊音按输入习惯自动召回,保留一个总开关用于排障或个人偏好。
//! 保存写 `config.ini`,再经命名管道让常驻 Server 立即重载。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    imp::run();
    #[cfg(not(windows))]
    println!("lueying-settings 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;

    use gpui::{
        App, Bounds, ClickEvent, Context, FocusHandle, FontWeight, KeyDownEvent, Point,
        SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, div, prelude::*, px,
        rgb, size,
    };
    use gpui_platform::application;
    use lueying_engine::Fuzzy;
    use lueying_protocol::{Reply, Request, pipe_name};
    use lueying_server::config::{self, AppConfig};
    use lueying_server::theme;
    use lueying_server::update::{self, Check, Release};
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_OVERWRITEPROMPT,
        OFN_PATHMUSTEXIST, OPEN_FILENAME_FLAGS, OPENFILENAMEW,
    };
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, IDOK, MB_DEFBUTTON2, MB_ICONQUESTION, MB_OKCANCEL, MessageBoxW,
        SM_CXSCREEN, SM_CYSCREEN,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    const WIDTH: f32 = 460.0;
    /// 三组设置 + 页脚;480 逻辑像素在 768p、150% 缩放下也放得下。
    const HEIGHT: f32 = 480.0;

    #[derive(Clone)]
    enum UpdateState {
        Checking,
        Latest,
        NoRelease,
        Available(Release),
        Failed(String),
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Notice {
        None,
        Info,
        Success,
        Error,
    }

    #[derive(Clone, Copy)]
    enum ButtonKind {
        Primary,
        Secondary,
        Ghost,
        Danger,
    }

    struct SettingsPanel {
        cfg: AppConfig,
        path: Option<PathBuf>,
        /// 模糊组当前适应次数,来自常驻 Server。
        groups: Vec<(String, u32)>,
        message: String,
        notice: Notice,
        /// 清空词库需要点两次,防误触。
        confirm_clear: bool,
        /// 关闭窗口的回调只登记一次。
        close_hooked: bool,
        /// 安装目录里的版本,打开设置窗时读一次。
        current: String,
        updates: UpdateState,
        /// 让 Esc / Ctrl+S 在设置窗任意位置都能用。
        focus: FocusHandle,
    }

    pub(super) fn run() {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let path = config::path();
        let cfg = path
            .as_ref()
            .map_or_else(AppConfig::default, |path| config::load(path));
        application().run(move |cx: &mut App| {
            // 主屏居中;按系统 DPI 把物理像素换成 GPUI 的逻辑像素。
            let scale = unsafe { GetDpiForSystem() }.max(96) as f32 / 96.0;
            let screen = unsafe {
                (
                    GetSystemMetrics(SM_CXSCREEN) as f32,
                    GetSystemMetrics(SM_CYSCREEN) as f32,
                )
            };
            let bounds = Bounds {
                origin: Point::new(
                    px(((screen.0 / scale - WIDTH) / 2.0).max(0.0)),
                    px(((screen.1 / scale - HEIGHT) / 2.0).max(0.0)),
                ),
                size: size(px(WIDTH), px(HEIGHT)),
            };
            let path = path.clone();
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("掠影设置".into()),
                        ..Default::default()
                    }),
                    focus: true,
                    is_resizable: false,
                    ..Default::default()
                },
                move |_, cx| {
                    cx.new(|cx| {
                        let mut panel = SettingsPanel {
                            cfg: cfg.clone(),
                            path: path.clone(),
                            groups: call(&Request::FuzzyStats)
                                .map(|reply| reply.groups)
                                .unwrap_or_default(),
                            message: String::new(),
                            notice: Notice::None,
                            confirm_clear: false,
                            close_hooked: false,
                            current: update::current_version(),
                            updates: UpdateState::Checking,
                            focus: cx.focus_handle(),
                        };
                        panel.check_updates(cx);
                        panel
                    })
                },
            );
            if let Ok(handle) = &opened {
                let _ = handle.update(cx, |panel, window, cx| {
                    window.focus(&panel.focus, cx);
                });
            } else {
                cx.quit();
            }
        });
    }

    impl SettingsPanel {
        fn clear_notice(&mut self) {
            self.notice = Notice::None;
            self.message.clear();
            self.confirm_clear = false;
        }

        fn say(&mut self, notice: Notice, message: impl Into<String>) {
            // 除了「再点一次确认清空」,任何结果提示都结束清空确认态。
            if notice != Notice::Info {
                self.confirm_clear = false;
            }
            self.notice = notice;
            self.message = message.into();
        }

        fn save(&mut self) {
            self.confirm_clear = false;
            let Some(path) = &self.path else {
                self.say(Notice::Error, "没有可写的配置路径");
                return;
            };
            match config::save(path, &self.cfg) {
                Ok(()) => {
                    reload();
                    self.say(Notice::Success, "已保存");
                }
                Err(e) => self.say(Notice::Error, format!("保存失败: {e}")),
            }
        }

        fn export_user(&mut self) {
            self.confirm_clear = false;
            let Some(path) = choose_user_file(true) else {
                return;
            };
            match call(&Request::ExportUser) {
                Some(reply) => match std::fs::write(&path, reply.user) {
                    Ok(()) => self.say(Notice::Success, format!("已导出到 {}", path.display())),
                    Err(e) => self.say(Notice::Error, format!("导出失败: {e}")),
                },
                None => self.say(Notice::Error, "输入法服务未运行,启动后再试"),
            }
        }

        fn import_user(&mut self) {
            self.confirm_clear = false;
            let Some(path) = choose_user_file(false) else {
                return;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => {
                    self.say(Notice::Error, format!("读取失败: {e}"));
                    return;
                }
            };
            match call(&Request::ImportUser { text }) {
                Some(reply) => {
                    self.groups = reply.groups;
                    self.say(Notice::Success, format!("已从 {} 合并", path.display()));
                }
                None => self.say(Notice::Error, "输入法服务未运行,启动后再试"),
            }
        }

        /// 后台查 GitHub Release;网络慢不能卡住设置窗。
        fn check_updates(&mut self, cx: &mut Context<Self>) {
            self.clear_notice();
            self.updates = UpdateState::Checking;
            let task = cx
                .background_executor()
                .spawn(async move { update::check() });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |panel, cx| {
                    panel.updates = match result {
                        Ok(Check::Latest) => UpdateState::Latest,
                        Ok(Check::Available(release)) => UpdateState::Available(release),
                        Ok(Check::NoRelease) => UpdateState::NoRelease,
                        Err(error) => UpdateState::Failed(error),
                    };
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }

        /// 确认后把更新器复制到临时目录并启动;设置窗退出,更新器接管安装。
        fn install_update(&mut self, release: Release, cx: &mut Context<Self>) {
            self.confirm_clear = false;
            let text = format!(
                "安装掠影 {}？\n\n设置窗口会关闭,输入法服务短暂重启。",
                release.version
            );
            let confirmed = unsafe {
                MessageBoxW(
                    None,
                    &HSTRING::from(text),
                    &HSTRING::from("检查更新"),
                    MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
                ) == IDOK
            };
            if !confirmed {
                return;
            }
            match update::launch_updater(&release, std::process::id()) {
                Ok(()) => cx.quit(),
                Err(error) => {
                    self.say(Notice::Error, format!("启动更新失败: {error}"));
                    cx.notify();
                }
            }
        }
    }

    /// 给常驻 Server 发一条请求并取回应答;没在运行返回 None。
    fn call(request: &Request) -> Option<Reply> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name())
            .ok()?;
        let mut line = serde_json::to_string(request).ok()?;
        line.push('\n');
        file.write_all(line.as_bytes()).ok()?;
        file.flush().ok()?;
        let mut reply = String::new();
        BufReader::new(file).read_line(&mut reply).ok()?;
        serde_json::from_str(&reply).ok()
    }

    /// 给常驻 Server 发一条请求;没在运行返回 false。
    fn send(request: &Request) -> bool {
        call(request).is_some()
    }

    /// 通知常驻 Server 重读配置。
    fn reload() {
        let _ = send(&Request::ReloadConfig);
    }

    /// 系统文件对话框;取消或失败时返回 None。
    fn choose_user_file(save: bool) -> Option<PathBuf> {
        let mut buf = [0u16; 1024];
        if save {
            for (slot, ch) in buf.iter_mut().zip("lueying-user.tsv".encode_utf16()) {
                *slot = ch;
            }
        }
        let filter: Vec<u16> = "用户词库 (*.tsv)\0*.tsv\0所有文件 (*.*)\0*.*\0\0"
            .encode_utf16()
            .collect();
        let title: Vec<u16> = if save {
            "导出用户词库"
        } else {
            "导入用户词库"
        }
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
        let extension: Vec<u16> = "tsv\0".encode_utf16().collect();
        let mut dialog = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: PWSTR(buf.as_mut_ptr()),
            nMaxFile: buf.len() as u32,
            lpstrTitle: PCWSTR(title.as_ptr()),
            Flags: OFN_FILEMUSTEXIST
                | OFN_PATHMUSTEXIST
                | if save {
                    OFN_OVERWRITEPROMPT
                } else {
                    OPEN_FILENAME_FLAGS(0)
                },
            lpstrDefExt: PCWSTR(extension.as_ptr()),
            ..Default::default()
        };
        let picked = unsafe {
            if save {
                GetSaveFileNameW(&mut dialog)
            } else {
                GetOpenFileNameW(&mut dialog)
            }
        };
        if !picked.as_bool() {
            return None;
        }
        let len = buf.iter().position(|&ch| ch == 0).unwrap_or(buf.len());
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }

    fn section_label(title: &'static str, colors: theme::Colors, first: bool) -> impl IntoElement {
        div()
            .px(px(14.0))
            .pt(px(10.0))
            .pb(px(4.0))
            .when(!first, |el| {
                el.border_t_1().border_color(rgb(colors.border))
            })
            .text_size(px(11.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(colors.text_tertiary))
            .child(title)
    }

    fn setting_row(
        label: &'static str,
        detail: impl Into<SharedString>,
        control: impl IntoElement,
        colors: theme::Colors,
        last: bool,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .h(px(40.0))
            .px(px(14.0))
            .when(!last, |el| el.border_b_1().border_color(rgb(colors.border)))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .flex_col()
                    .gap(px(1.0))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(rgb(colors.text))
                            .child(label),
                    )
                    .child(
                        div()
                            .w_full()
                            .max_w(px(230.0))
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(rgb(colors.text_secondary))
                            .child(detail.into()),
                    ),
            )
            .child(control)
    }

    fn toggle(
        id: &'static str,
        on: bool,
        colors: theme::Colors,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .w(px(38.0))
            .h(px(22.0))
            .p(px(3.0))
            .rounded_full()
            .cursor_pointer()
            .bg(rgb(if on { colors.accent } else { colors.off_bg }))
            .when(on, |el| el.justify_end())
            .when(!on, |el| el.justify_start())
            .hover(|style| style.opacity(0.9))
            .active(|style| style.opacity(0.75))
            .on_click(on_click)
            .child(
                div()
                    .w(px(16.0))
                    .h(px(16.0))
                    .rounded_full()
                    .bg(rgb(if on {
                        colors.on_accent
                    } else {
                        colors.knob_off
                    }))
                    .shadow_xs(),
            )
    }

    fn segment(
        value: usize,
        selected: bool,
        high_contrast: bool,
        colors: theme::Colors,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let (selected_bg, selected_text) = if high_contrast {
            (colors.accent, colors.on_accent)
        } else {
            (colors.segment, colors.text)
        };
        div()
            .id(("page-size", value))
            .flex()
            .items_center()
            .justify_center()
            .w(px(34.0))
            .h(px(26.0))
            .rounded_md()
            .text_size(px(12.0))
            .cursor_pointer()
            .when(selected, |el| {
                el.bg(rgb(selected_bg))
                    .text_color(rgb(selected_text))
                    .when(!high_contrast, |el| el.shadow_xs())
            })
            .when(!selected, |el| {
                el.text_color(rgb(colors.text_secondary))
                    .hover(|style| style.text_color(rgb(colors.text)))
            })
            .active(|style| style.opacity(0.8))
            .on_click(on_click)
            .child(value.to_string())
    }

    fn button(
        id: &'static str,
        label: impl Into<SharedString>,
        kind: ButtonKind,
        enabled: bool,
        colors: theme::Colors,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let label: SharedString = label.into();
        let (bg, text) = match kind {
            ButtonKind::Primary => (colors.accent, colors.on_accent),
            ButtonKind::Secondary => (colors.surface, colors.text),
            ButtonKind::Ghost => (colors.surface, colors.text_secondary),
            ButtonKind::Danger => (colors.danger_soft, colors.danger),
        };
        let hover = match kind {
            ButtonKind::Primary => colors.accent_hover,
            ButtonKind::Secondary => colors.hover,
            // Ghost 没有底色,悬停要用比窗口底更深一档的中性色才看得见。
            ButtonKind::Ghost => colors.border,
            ButtonKind::Danger => colors.danger_soft,
        };
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(30.0))
            .px(px(12.0))
            .rounded_md()
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .when(!matches!(kind, ButtonKind::Ghost), |el| el.bg(rgb(bg)))
            .text_color(rgb(if enabled { text } else { colors.text_tertiary }))
            .when(matches!(kind, ButtonKind::Secondary), |el| {
                el.border_1().border_color(rgb(colors.border_strong))
            })
            .when(enabled, |el| {
                el.cursor_pointer()
                    .when(matches!(kind, ButtonKind::Danger), |el| {
                        el.hover(|style| style.opacity(0.9))
                    })
                    .when(!matches!(kind, ButtonKind::Danger), |el| {
                        el.hover(move |style| style.bg(rgb(hover)))
                    })
                    .active(|style| style.opacity(0.8))
                    .on_click(on_click)
            })
            .child(label)
    }

    fn notice_color(notice: Notice, colors: theme::Colors) -> u32 {
        match notice {
            Notice::Success => colors.success,
            Notice::Error => colors.danger,
            Notice::None | Notice::Info => colors.text_secondary,
        }
    }

    /// 组名转为设置页可读的模糊对。
    fn group_label(name: &str) -> &'static str {
        match name {
            "zcs" => "z/zh c/ch s/sh",
            "ln" => "l/n",
            "fh" => "f/h",
            "rl" => "r/l",
            "an_ang" => "an/ang",
            "en_eng" => "en/eng",
            "in_ing" => "in/ing",
            _ => "其他",
        }
    }

    impl Render for SettingsPanel {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // 关掉设置窗就退出进程;gpui 默认会在最后一个窗口关闭后留下空事件循环。
            if !self.close_hooked {
                self.close_hooked = true;
                window.on_window_should_close(&*cx, |_, cx| {
                    cx.quit();
                    true
                });
            }
            let appearance = theme::Appearance::of(window);
            let colors = theme::colors(appearance);
            let high_contrast = matches!(appearance, theme::Appearance::HighContrast);
            let page = self.cfg.page_size;
            let (update_label, update_kind, update_enabled) = match &self.updates {
                UpdateState::Checking => ("检查中".to_string(), ButtonKind::Ghost, false),
                UpdateState::Latest => ("已是最新".to_string(), ButtonKind::Ghost, true),
                UpdateState::NoRelease => ("检查更新".to_string(), ButtonKind::Ghost, true),
                UpdateState::Available(release) => (
                    format!("安装 {}", release.version),
                    ButtonKind::Primary,
                    true,
                ),
                UpdateState::Failed(_) => ("检查失败 · 重试".to_string(), ButtonKind::Ghost, true),
            };
            let adapted = self
                .groups
                .iter()
                .filter(|(_, count)| *count > 0)
                .map(|(name, count)| format!("{} ×{count}", group_label(name)))
                .collect::<Vec<_>>()
                .join(" · ");
            let adapted = if adapted.is_empty() {
                "按输入习惯学习模糊音".to_string()
            } else {
                adapted
            };
            let reset_enabled = self.groups.iter().any(|(_, count)| *count > 0);
            let (notice_text, notice_kind) = if !self.message.is_empty() {
                (self.message.clone(), self.notice)
            } else if let UpdateState::Failed(error) = &self.updates {
                (format!("检查更新失败: {error}"), Notice::Error)
            } else {
                ("修改后点保存生效".to_string(), Notice::None)
            };
            let notice_tint = notice_color(notice_kind, colors);
            let version = format!("拼音输入法 · v{}", self.current);

            let page_control = div()
                .flex()
                .items_center()
                .gap(px(2.0))
                .p(px(2.0))
                .rounded_md()
                .bg(rgb(if high_contrast {
                    colors.surface
                } else {
                    colors.surface_alt
                }))
                .border_1()
                .border_color(rgb(colors.border))
                .children([3usize, 5, 9].into_iter().map(|n| {
                    segment(
                        n,
                        page == n,
                        high_contrast,
                        colors,
                        cx.listener(move |this, _, _, cx| {
                            this.cfg.page_size = n;
                            this.clear_notice();
                            cx.notify();
                        }),
                    )
                }));

            let card = div()
                .flex()
                .flex_col()
                .bg(rgb(colors.surface))
                .border_1()
                .border_color(rgb(colors.border))
                .rounded_lg()
                .overflow_hidden()
                .when(!high_contrast, |el| el.shadow_xs())
                .child(section_label("输入", colors, true))
                .child(setting_row(
                    "中文标点",
                    "按中文习惯输出 ，。；：“”",
                    toggle(
                        "chinese-punct",
                        self.cfg.chinese_punct,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.cfg.chinese_punct = !this.cfg.chinese_punct;
                            this.clear_notice();
                            cx.notify();
                        }),
                    ),
                    colors,
                    false,
                ))
                .child(setting_row(
                    "Shift 单击切中英",
                    "按住 Shift 输入大写，单击切换",
                    toggle(
                        "shift-toggle",
                        self.cfg.shift_toggle,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.cfg.shift_toggle = !this.cfg.shift_toggle;
                            this.clear_notice();
                            cx.notify();
                        }),
                    ),
                    colors,
                    false,
                ))
                .child(setting_row(
                    "输出繁体",
                    "候选与上屏使用繁体",
                    toggle(
                        "traditional",
                        self.cfg.traditional,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.cfg.traditional = !this.cfg.traditional;
                            this.clear_notice();
                            cx.notify();
                        }),
                    ),
                    colors,
                    true,
                ))
                .child(section_label("候选", colors, false))
                .child(setting_row(
                    "每页候选数",
                    "按屏幕宽度选择",
                    page_control,
                    colors,
                    false,
                ))
                .child(setting_row(
                    "自动模糊音",
                    adapted,
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(button(
                            "reset-fuzzy",
                            "重置",
                            ButtonKind::Ghost,
                            reset_enabled,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                match call(&Request::ResetFuzzy) {
                                    Some(reply) => {
                                        this.groups = reply.groups;
                                        this.say(Notice::Success, "已重置模糊音组统计");
                                    }
                                    None => this.say(Notice::Error, "输入法服务未运行,启动后再试"),
                                }
                                cx.notify();
                            }),
                        ))
                        .child(toggle(
                            "fuzzy",
                            !self.cfg.fuzzy.is_off(),
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.cfg.fuzzy = if this.cfg.fuzzy.is_off() {
                                    Fuzzy::auto()
                                } else {
                                    Fuzzy::off()
                                };
                                this.clear_notice();
                                cx.notify();
                            }),
                        )),
                    colors,
                    true,
                ))
                .child(section_label("词库", colors, false))
                .child(setting_row(
                    "用户词库",
                    "导入、导出或清空本机学习记录",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(button(
                            "import-user",
                            "导入",
                            ButtonKind::Secondary,
                            true,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.import_user();
                                cx.notify();
                            }),
                        ))
                        .child(button(
                            "export-user",
                            "导出",
                            ButtonKind::Secondary,
                            true,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.export_user();
                                cx.notify();
                            }),
                        ))
                        .child(button(
                            "clear-user",
                            if self.confirm_clear {
                                "确认清空"
                            } else {
                                "清空"
                            },
                            if self.confirm_clear {
                                ButtonKind::Danger
                            } else {
                                ButtonKind::Ghost
                            },
                            true,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                if this.confirm_clear {
                                    this.confirm_clear = false;
                                    if send(&Request::ClearUser) {
                                        this.say(Notice::Success, "已清空用户词库");
                                    } else {
                                        this.say(Notice::Error, "输入法服务未运行,启动后再试");
                                    }
                                } else {
                                    this.confirm_clear = true;
                                    this.say(Notice::Info, "再点一次确认清空");
                                }
                                cx.notify();
                            }),
                        )),
                    colors,
                    true,
                ));

            let header = div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(36.0))
                        .h(px(36.0))
                        .rounded_lg()
                        .bg(rgb(colors.accent))
                        .text_size(px(18.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(colors.on_accent))
                        .child("掠"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .min_w(px(0.0))
                        .max_w(px(230.0))
                        .child(
                            div()
                                .text_size(px(19.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(colors.text))
                                .child("掠影"),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(px(12.0))
                                .text_color(rgb(colors.text_secondary))
                                .child(version),
                        ),
                )
                .child(div().flex_1())
                .child(button(
                    "update",
                    update_label,
                    update_kind,
                    update_enabled,
                    colors,
                    cx.listener(|this, _, _, cx| match this.updates.clone() {
                        UpdateState::Available(release) => this.install_update(release, cx),
                        UpdateState::Checking => {}
                        UpdateState::Latest | UpdateState::NoRelease | UpdateState::Failed(_) => {
                            this.check_updates(cx);
                            cx.notify();
                        }
                    }),
                ));

            let footer = div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(notice_kind != Notice::None, |el| {
                            el.child(
                                div()
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded_full()
                                    .bg(rgb(notice_tint)),
                            )
                        })
                        .child(
                            div()
                                .max_w(px(280.0))
                                .truncate()
                                .text_size(px(12.0))
                                .text_color(rgb(notice_tint))
                                .child(notice_text),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(button(
                            "close",
                            "关闭",
                            ButtonKind::Secondary,
                            true,
                            colors,
                            cx.listener(|_, _, _, cx| cx.quit()),
                        ))
                        .child(button(
                            "save",
                            "保存",
                            ButtonKind::Primary,
                            true,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.save();
                                cx.notify();
                            }),
                        )),
                );

            div()
                .id("settings-root")
                .font_family("Microsoft YaHei UI")
                .flex()
                .flex_col()
                .gap(px(12.0))
                .p(px(18.0))
                .w(px(WIDTH))
                .h(px(HEIGHT))
                .bg(rgb(colors.bg))
                .text_color(rgb(colors.text))
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        cx.quit();
                    } else if event.keystroke.modifiers.control && event.keystroke.key == "s" {
                        this.save();
                        cx.notify();
                    }
                }))
                .child(header)
                .child(card)
                .child(div().flex_1())
                .child(footer)
                .into_any_element()
        }
    }
}
