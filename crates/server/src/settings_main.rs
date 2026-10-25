//! 设置窗独立进程:候选数、中文标点、Shift 切中英。
//! 模糊音按输入习惯自动召回,保留一个总开关用于排障或个人偏好。
//! 保存写 `config.ini`,再经命名管道让常驻 Server 立即重载。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    imp::run();
    #[cfg(not(windows))]
    println!("pinyin-settings 仅支持 Windows");
}

#[cfg(windows)]
mod imp {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;

    use gpui::{
        App, Bounds, Context, MouseButton, MouseDownEvent, Point, SharedString, TitlebarOptions,
        Window, WindowAppearance, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
    };
    use gpui_platform::application;
    use pinyin_engine::Fuzzy;
    use pinyin_protocol::{Reply, Request, pipe_name};
    use pinyin_server::config::{self, AppConfig};

    const WIDTH: f32 = 440.0;
    /// 6 行设置 + 标题/说明/底部按钮;留足行高与间距,避免底部被裁。
    const HEIGHT: f32 = 600.0;

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
    }

    pub(super) fn run() {
        let path = config::path();
        let cfg = path
            .as_ref()
            .map_or_else(AppConfig::default, |path| config::load(path));
        application().run(move |cx: &mut App| {
            let bounds = Bounds {
                origin: Point::new(px(260.0), px(160.0)),
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
                    cx.new(|_| SettingsPanel {
                        cfg: cfg.clone(),
                        path: path.clone(),
                        groups: call(&Request::FuzzyStats)
                            .map(|reply| reply.groups)
                            .unwrap_or_default(),
                        message: String::new(),
                        confirm_clear: false,
                        close_hooked: false,
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

    #[derive(Clone, Copy)]
    struct Palette {
        bg: u32,
        card: u32,
        border: u32,
        text: u32,
        muted: u32,
        accent: u32,
        off_bg: u32,
        off_text: u32,
    }

    fn palette(dark: bool) -> Palette {
        if dark {
            Palette {
                bg: 0x1f1f24,
                card: 0x2b2b31,
                border: 0x44444d,
                text: 0xe8e8ee,
                muted: 0xa0a0aa,
                accent: 0x3b78ff,
                off_bg: 0x3a3a42,
                off_text: 0xd0d0d8,
            }
        } else {
            Palette {
                bg: 0xf2f2f5,
                card: 0xffffff,
                border: 0xd9d9e0,
                text: 0x20202a,
                muted: 0x70707a,
                accent: 0x2f6fed,
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
        on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .min_w(px(42.0))
            .px_2()
            .py_1()
            .rounded_md()
            .text_size(px(13.0))
            .text_center()
            .bg(rgb(if on { colors.accent } else { colors.off_bg }))
            .text_color(rgb(if on { 0xffffff } else { colors.off_text }))
            .on_mouse_down(MouseButton::Left, on_click)
            .child(label.into())
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
            let colors = palette(matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ));
            let page = self.cfg.page_size;
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
                    pill(
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
                    ),
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
                        .text_size(px(11.0))
                        .text_color(rgb(colors.muted))
                        .child(format!("版本 {}", env!("CARGO_PKG_VERSION"))),
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
