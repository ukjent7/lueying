//! 设置窗独立进程:候选数、中文标点、Shift 切中英。
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
        App, Bounds, ClickEvent, Context, Point, SharedString, TitlebarOptions, Window,
        WindowAppearance, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
    };
    use gpui_platform::application;
    use lueying_engine::Fuzzy;
    use lueying_protocol::{Reply, Request, pipe_name};
    use lueying_server::config::{self, AppConfig};
    use lueying_server::theme;
    use lueying_server::update::{self, Check, Release};
    use windows::Win32::Graphics::Gdi::{
        COLOR_3DFACE, COLOR_BTNTEXT, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT,
        COLOR_WINDOW, COLOR_WINDOWFRAME, COLOR_WINDOWTEXT,
    };
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

    const WIDTH: f32 = 440.0;
    /// 6 行设置 + 标题/说明/更新行/底部按钮;440 逻辑像素在 768p、150% 缩放下也放得下。
    const HEIGHT: f32 = 440.0;

    #[derive(Clone)]
    enum UpdateState {
        Checking,
        Latest,
        NoRelease,
        Available(Release),
        Failed(String),
    }

    struct SettingsPanel {
        cfg: AppConfig,
        path: Option<PathBuf>,
        /// 模糊组当前适应次数,来自常驻 Server。
        groups: Vec<(String, u32)>,
        message: String,
        /// 清空词库需要点两次,防误触。
        confirm_clear: bool,
        /// 关闭窗口的回调只登记一次。
        close_hooked: bool,
        /// 安装目录里的版本,打开设置窗时读一次。
        current: String,
        updates: UpdateState,
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
                            confirm_clear: false,
                            close_hooked: false,
                            current: update::current_version(),
                            updates: UpdateState::Checking,
                        };
                        panel.check_updates(cx);
                        panel
                    })
                },
            );
            if opened.is_err() {
                cx.quit();
            }
        });
    }

    impl SettingsPanel {
        fn save(&mut self) {
            let Some(path) = &self.path else {
                self.message = "没有可写的配置路径".into();
                return;
            };
            match config::save(path, &self.cfg) {
                Ok(()) => {
                    reload();
                    self.message = "已保存".into();
                }
                Err(e) => self.message = format!("保存失败: {e}"),
            }
        }

        fn export_user(&mut self) {
            let Some(path) = choose_user_file(true) else {
                return;
            };
            self.message = match call(&Request::ExportUser) {
                Some(reply) => match std::fs::write(&path, reply.user) {
                    Ok(()) => format!("已导出到 {}", path.display()),
                    Err(e) => format!("导出失败: {e}"),
                },
                None => "输入法服务未运行,启动后再试".into(),
            };
        }

        fn import_user(&mut self) {
            let Some(path) = choose_user_file(false) else {
                return;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => {
                    self.message = format!("读取失败: {e}");
                    return;
                }
            };
            self.message = match call(&Request::ImportUser { text }) {
                Some(reply) => {
                    self.groups = reply.groups;
                    format!("已从 {} 合并", path.display())
                }
                None => "输入法服务未运行,启动后再试".into(),
            };
        }

        /// 后台查 GitHub Release;网络慢不能卡住设置窗。
        fn check_updates(&mut self, cx: &mut Context<Self>) {
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
                    self.message = format!("启动更新失败: {error}");
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

    #[derive(Clone, Copy)]
    struct Palette {
        bg: u32,
        card: u32,
        border: u32,
        text: u32,
        muted: u32,
        accent: u32,
        /// 选中胶囊上的文字;高对比度下必须跟随系统高亮文字色。
        on_accent: u32,
        off_bg: u32,
        off_text: u32,
    }

    fn system_palette() -> Palette {
        Palette {
            bg: theme::system_color(COLOR_WINDOW),
            card: theme::system_color(COLOR_WINDOW),
            border: theme::system_color(COLOR_WINDOWFRAME),
            text: theme::system_color(COLOR_WINDOWTEXT),
            muted: theme::system_color(COLOR_GRAYTEXT),
            accent: theme::system_color(COLOR_HIGHLIGHT),
            on_accent: theme::system_color(COLOR_HIGHLIGHTTEXT),
            off_bg: theme::system_color(COLOR_3DFACE),
            off_text: theme::system_color(COLOR_BTNTEXT),
        }
    }

    fn palette(dark: bool) -> Palette {
        if dark {
            Palette {
                bg: 0x1f1f24,
                card: 0x2b2b31,
                border: 0x44444d,
                text: 0xe8e8ee,
                muted: 0xa0a0aa,
                // 深色主题里白字在 0x3b78ff 上只有 3.95:1,压到 0x2f6fed 才到 4.5:1。
                accent: 0x2f6fed,
                on_accent: 0xffffff,
                off_bg: 0x3a3a42,
                off_text: 0xd0d0d8,
            }
        } else {
            Palette {
                bg: 0xf2f2f5,
                card: 0xffffff,
                border: 0xd9d9e0,
                text: 0x20202a,
                // 旧值 0x70707a 在浅色底上只有 4.38:1,略低于小字下限。
                muted: 0x686872,
                accent: 0x2f6fed,
                on_accent: 0xffffff,
                off_bg: 0xe6e6ec,
                off_text: 0x50505a,
            }
        }
    }

    fn row(label: &'static str, value: impl IntoElement, colors: Palette) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(34.0))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(rgb(colors.text))
                    .child(label),
            )
            .child(value)
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

    fn pill(
        label: impl Into<SharedString>,
        on: bool,
        colors: Palette,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let label: SharedString = label.into();
        div()
            // `active` 只挂在有状态元素上;标签在同类按钮里唯一。
            .id(label.clone())
            .min_w(px(42.0))
            .px_2()
            .py_1()
            .rounded_md()
            .text_size(px(13.0))
            .text_center()
            .cursor_pointer()
            .bg(rgb(if on { colors.accent } else { colors.off_bg }))
            .text_color(rgb(if on {
                colors.on_accent
            } else {
                colors.off_text
            }))
            .hover(|style| style.opacity(0.9))
            .active(|style| style.opacity(0.78))
            // 按钮在鼠标松开时触发,按下后移出可以取消,和系统按钮一致。
            .on_click(on_click)
            .child(label)
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
            let colors = if theme::high_contrast() {
                system_palette()
            } else {
                palette(matches!(
                    window.appearance(),
                    WindowAppearance::Dark | WindowAppearance::VibrantDark
                ))
            };
            let page = self.cfg.page_size;
            let (update_text, update_label, update_active) = match &self.updates {
                UpdateState::Checking => ("正在检查更新…".to_string(), "检查中".to_string(), false),
                UpdateState::Latest => (
                    format!("版本 {} · 已是最新", self.current),
                    "检查更新".to_string(),
                    false,
                ),
                UpdateState::NoRelease => (
                    format!("版本 {} · 暂无发布版本", self.current),
                    "检查更新".to_string(),
                    false,
                ),
                UpdateState::Available(release) => (
                    format!("版本 {} → {}", self.current, release.version),
                    format!("安装 {}", release.version),
                    true,
                ),
                UpdateState::Failed(error) => (
                    format!("版本 {} · 检查失败: {error}", self.current),
                    "重试".to_string(),
                    false,
                ),
            };
            let mut card = div()
                .flex()
                .flex_col()
                .px_4()
                .bg(rgb(colors.card))
                .border_1()
                .border_color(rgb(colors.border))
                .rounded_lg()
                .child(row(
                    "每页候选数",
                    div()
                        .flex()
                        .gap_1()
                        .children([3usize, 5, 9].into_iter().map(|n| {
                            pill(
                                n.to_string(),
                                page == n,
                                colors,
                                cx.listener(move |this, _, _, cx| {
                                    this.cfg.page_size = n;
                                    this.message.clear();
                                    cx.notify();
                                }),
                            )
                        })),
                    colors,
                ))
                .child(row(
                    "中文标点",
                    pill(
                        if self.cfg.chinese_punct { "开" } else { "关" },
                        self.cfg.chinese_punct,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.cfg.chinese_punct = !this.cfg.chinese_punct;
                            this.message.clear();
                            cx.notify();
                        }),
                    ),
                    colors,
                ))
                .child(row(
                    "Shift 单击切中英",
                    pill(
                        if self.cfg.shift_toggle { "开" } else { "关" },
                        self.cfg.shift_toggle,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.cfg.shift_toggle = !this.cfg.shift_toggle;
                            this.message.clear();
                            cx.notify();
                        }),
                    ),
                    colors,
                ))
                .child(row(
                    "自动模糊音",
                    pill(
                        if self.cfg.fuzzy.is_off() {
                            "关"
                        } else {
                            "开"
                        },
                        !self.cfg.fuzzy.is_off(),
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.cfg.fuzzy = if this.cfg.fuzzy.is_off() {
                                Fuzzy::auto()
                            } else {
                                Fuzzy::off()
                            };
                            this.message.clear();
                            cx.notify();
                        }),
                    ),
                    colors,
                ))
                .child(row(
                    "用户词库",
                    div()
                        .flex()
                        .gap_1()
                        .child(pill(
                            "导出",
                            false,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.export_user();
                                cx.notify();
                            }),
                        ))
                        .child(pill(
                            "导入",
                            false,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.import_user();
                                cx.notify();
                            }),
                        ))
                        .child(pill(
                            if self.confirm_clear {
                                "确认清空"
                            } else {
                                "清空"
                            },
                            self.confirm_clear,
                            colors,
                            cx.listener(|this, _, _, cx| {
                                if this.confirm_clear {
                                    this.confirm_clear = false;
                                    this.message = if send(&Request::ClearUser) {
                                        "已清空用户词库".into()
                                    } else {
                                        "输入法服务未运行,启动后再试".into()
                                    };
                                } else {
                                    this.confirm_clear = true;
                                    this.message = "再点一次确认清空".into();
                                }
                                cx.notify();
                            }),
                        )),
                    colors,
                ));

            let adapted = self
                .groups
                .iter()
                .filter(|(_, count)| *count > 0)
                .map(|(name, count)| format!("{} ×{count}", group_label(name)))
                .collect::<Vec<_>>()
                .join(" · ");
            let adapted = if adapted.is_empty() {
                "自动学习".to_string()
            } else {
                adapted
            };
            card = card.child(row(
                "模糊音",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .max_w(px(220.0))
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(rgb(colors.muted))
                            .child(adapted),
                    )
                    .child(pill(
                        "重置",
                        false,
                        colors,
                        cx.listener(|this, _, _, cx| {
                            match call(&Request::ResetFuzzy) {
                                Some(reply) => {
                                    this.groups = reply.groups;
                                    this.message = "已重置模糊音组统计".into();
                                }
                                None => {
                                    this.message = "输入法服务未运行,启动后再试".into();
                                }
                            }
                            cx.notify();
                        }),
                    )),
                colors,
            ));

            div()
                .font_family("Microsoft YaHei")
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .w(px(WIDTH))
                .h(px(HEIGHT))
                .bg(rgb(colors.bg))
                .text_color(rgb(colors.text))
                .child(div().text_size(px(18.0)).child("掠影设置"))
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(colors.muted))
                        .child("修改后点保存,当前输入法窗口立即生效。"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .max_w(px(300.0))
                                .truncate()
                                .text_size(px(12.0))
                                .text_color(rgb(colors.muted))
                                .child(update_text),
                        )
                        .child(pill(
                            update_label,
                            update_active,
                            colors,
                            cx.listener(|this, _, _, cx| match this.updates.clone() {
                                UpdateState::Available(release) => {
                                    this.install_update(release, cx);
                                }
                                UpdateState::Checking => {}
                                UpdateState::Latest
                                | UpdateState::NoRelease
                                | UpdateState::Failed(_) => {
                                    this.check_updates(cx);
                                    cx.notify();
                                }
                            }),
                        )),
                )
                .child(card)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(colors.muted))
                                .child(self.message.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(pill(
                                    "关闭",
                                    false,
                                    colors,
                                    cx.listener(|_, _, _, cx| cx.quit()),
                                ))
                                .child(pill(
                                    "保存",
                                    true,
                                    colors,
                                    cx.listener(|this, _, _, cx| {
                                        this.save();
                                        cx.notify();
                                    }),
                                )),
                        ),
                )
                .into_any_element()
        }
    }
}
