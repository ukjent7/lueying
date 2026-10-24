//! 设置窗独立进程:候选数、模糊音、中文标点、Shift 切中英。
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
        App, Bounds, Context, MouseDownEvent, MouseButton, Point, SharedString, TitlebarOptions,
        Window, WindowAppearance, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
    };
    use gpui_platform::application;
    use pinyin_protocol::{Request, pipe_name};
    use pinyin_server::config::{self, AppConfig};

    const WIDTH: f32 = 440.0;
    /// 12 行设置 + 标题/说明/底部按钮;留足行高与间距,避免底部被裁。
    const HEIGHT: f32 = 640.0;

    struct SettingsPanel {
        cfg: AppConfig,
        path: Option<PathBuf>,
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
                        title: Some("拼音输入法设置".into()),
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

    /// 给常驻 Server 发一条请求;没在运行返回 false。
    fn send(request: &Request) -> bool {
        let Ok(mut file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_name())
        else {
            return false;
        };
        let Ok(mut line) = serde_json::to_string(request) else {
            return false;
        };
        line.push('\n');
        if file.write_all(line.as_bytes()).is_err() || file.flush().is_err() {
            return false;
        }
        let mut reply = String::new();
        BufReader::new(file).read_line(&mut reply).is_ok()
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
            let fuzzy = self.cfg.fuzzy;
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

            let toggles: [(&'static str, bool, fn(&mut AppConfig)); 6] = [
                ("模糊 z/zh c/ch s/sh", fuzzy.zcs, |c| c.fuzzy.zcs = !c.fuzzy.zcs),
                ("模糊 l/n", fuzzy.ln, |c| c.fuzzy.ln = !c.fuzzy.ln),
                ("模糊 f/h", fuzzy.fh, |c| c.fuzzy.fh = !c.fuzzy.fh),
                ("模糊 an/ang", fuzzy.an_ang, |c| {
                    c.fuzzy.an_ang = !c.fuzzy.an_ang
                }),
                ("模糊 en/eng", fuzzy.en_eng, |c| {
                    c.fuzzy.en_eng = !c.fuzzy.en_eng
                }),
                ("模糊 in/ing", fuzzy.in_ing, |c| {
                    c.fuzzy.in_ing = !c.fuzzy.in_ing
                }),
            ];
            for (label, value, flip) in toggles {
                card = card.child(row(
                    label,
                    pill(
                        if value { "开" } else { "关" },
                        value,
                        colors,
                        cx.listener(move |this, _, _, cx| {
                            flip(&mut this.cfg);
                            this.message.clear();
                            cx.notify();
                        }),
                    ),
                    colors,
                ));
            }

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
                .child(
                    div()
                        .text_size(px(18.0))
                        .child("拼音输入法设置"),
                )
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
