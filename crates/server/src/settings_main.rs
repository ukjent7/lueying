//! 设置窗独立进程:输入方案、候选外观、标点、词库数据与更新。
//! 保存写 `config.ini`,再经命名管道让常驻 Server 立即重载;
//! 短语、标点、按应用配置与备份在「词库」组里编辑/打包。

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
        App, Bounds, Context, FocusHandle, FontWeight, KeyDownEvent, Point, TitlebarOptions,
        Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
    };
    use gpui_platform::application;
    use lueying_engine::Fuzzy;
    use lueying_features::SchemeId;
    use lueying_protocol::{EngineStatus, Reply, Request, pipe_name};
    use lueying_server::config::{self, AppConfig, Channel, Region, UiFont, UiLayout};
    use lueying_server::theme;
    use lueying_server::update::{self, Check, Release};
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_OVERWRITEPROMPT,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, IDOK, MB_DEFBUTTON2, MB_ICONQUESTION, MB_OKCANCEL, MessageBoxW,
        SM_CXSCREEN, SM_CYSCREEN,
    };
    use windows::core::{HSTRING, PCWSTR, PWSTR};

    #[path = "settings_controls.rs"]
    mod controls;

    use controls::{ButtonKind, button, group_label, section_label, segment, setting_row, toggle};

    const WIDTH: f32 = 460.0;
    /// 四组设置 + 页脚;650 逻辑像素在 1080p 上留有余量,小屏按窗口高度收缩。
    const HEIGHT: f32 = 650.0;

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

    /// 设置窗里可用键盘操作的控件;KBD_ORDER 的顺序就是 Tab 遍历顺序。
    /// 分段组(字形地区、每页候选数、排列、字号、主题色、渠道)整组算
    /// 一个控件,用左右键改值。
    #[derive(Clone, Copy, PartialEq)]
    enum Kbd {
        Update,
        Scheme,
        Aux,
        ChinesePunct,
        ShiftToggle,
        Traditional,
        Region,
        PageSize,
        Layout,
        Font,
        Accent,
        Effects,
        EffectsToggle,
        Prediction,
        ResetFuzzy,
        Fuzzy,
        Phrases,
        PunctFile,
        AppsFile,
        ImportUser,
        ExportUser,
        BackupExport,
        BackupImport,
        ClearUser,
        Diagnostics,
        Channel,
        Close,
        Save,
    }

    const KBD_ORDER: [Kbd; 28] = [
        Kbd::Update,
        Kbd::Scheme,
        Kbd::Aux,
        Kbd::ChinesePunct,
        Kbd::ShiftToggle,
        Kbd::Traditional,
        Kbd::Region,
        Kbd::PageSize,
        Kbd::Layout,
        Kbd::Font,
        Kbd::Accent,
        Kbd::Effects,
        Kbd::EffectsToggle,
        Kbd::Prediction,
        Kbd::ResetFuzzy,
        Kbd::Fuzzy,
        Kbd::Phrases,
        Kbd::PunctFile,
        Kbd::AppsFile,
        Kbd::ImportUser,
        Kbd::ExportUser,
        Kbd::BackupExport,
        Kbd::BackupImport,
        Kbd::ClearUser,
        Kbd::Diagnostics,
        Kbd::Channel,
        Kbd::Close,
        Kbd::Save,
    ];

    struct SettingsPanel {
        cfg: AppConfig,
        path: Option<PathBuf>,
        /// 模糊组当前适应次数,来自常驻 Server。
        groups: Vec<(String, u32)>,
        /// 引擎资源状态(词典与模型加载情况);服务未运行时为 None。
        status: Option<EngineStatus>,
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
        /// 键盘焦点在 KBD_ORDER 里的下标;None 表示没有键盘焦点。
        kbd: Option<usize>,
        /// 按屏幕逻辑高度收缩后的窗口高度。
        height: f32,
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
            // 小屏或高缩放下按可用高度收缩,卡片内部滚动,页脚始终可见。
            let height = HEIGHT.min((screen.1 / scale - 24.0).max(480.0));
            let bounds = Bounds {
                origin: Point::new(
                    px(((screen.0 / scale - WIDTH) / 2.0).max(0.0)),
                    px(((screen.1 / scale - height) / 2.0).max(0.0)),
                ),
                size: size(px(WIDTH), px(height)),
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
                            status: call(&Request::Status).and_then(|reply| reply.status),
                            message: String::new(),
                            notice: Notice::None,
                            confirm_clear: false,
                            close_hooked: false,
                            current: update::current_version(),
                            updates: UpdateState::Checking,
                            focus: cx.focus_handle(),
                            kbd: None,
                            height,
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

        /// 当前键盘焦点是否落在某个控件上。
        fn kbd_is(&self, k: Kbd) -> bool {
            self.kbd.map_or(false, |i| KBD_ORDER[i] == k)
        }

        /// Tab / Shift+Tab 循环移动键盘焦点。
        fn kbd_step(&mut self, delta: isize) {
            let n = KBD_ORDER.len() as isize;
            let next = match self.kbd {
                Some(i) => (i as isize + delta).rem_euclid(n),
                None if delta > 0 => 0,
                None => n - 1,
            };
            self.kbd = Some(next as usize);
        }

        /// 字形地区分段组:左右键在选项间循环。
        fn region_shift(&mut self, delta: isize) {
            let n = Region::NAMES.len() as isize;
            let next = (self.cfg.region as isize + delta).rem_euclid(n) as usize;
            self.cfg.region = Region::from_index(next);
            self.clear_notice();
        }

        /// 输入方案分段组:左右键在全拼/小鹤/自然码/微软/搜狗之间循环。
        fn scheme_shift(&mut self, delta: isize) {
            let current = SchemeId::ALL
                .iter()
                .position(|&id| id == self.cfg.scheme)
                .unwrap_or(0);
            let next = (current as isize + delta).rem_euclid(SchemeId::ALL.len() as isize) as usize;
            self.cfg.scheme = SchemeId::ALL[next];
            self.clear_notice();
        }

        /// 每页候选数分段组:左右键在 3/5/9 之间循环。
        fn page_shift(&mut self, delta: isize) {
            const OPTIONS: [usize; 3] = [3, 5, 9];
            let current = OPTIONS
                .iter()
                .position(|&n| n == self.cfg.page_size)
                .unwrap_or(1);
            let next = (current as isize + delta).rem_euclid(OPTIONS.len() as isize) as usize;
            self.cfg.page_size = OPTIONS[next];
            self.clear_notice();
        }

        /// 候选排列分段组:左右键在横排/竖排之间切换。
        fn layout_shift(&mut self, delta: isize) {
            let current = if self.cfg.ui_layout == UiLayout::Vertical { 1 } else { 0 };
            let next = (current + delta).rem_euclid(2);
            self.cfg.ui_layout = UiLayout::from_index(next);
            self.clear_notice();
        }

        /// 候选字号分段组:左右键在小/标准/大之间循环。
        fn font_shift(&mut self, delta: isize) {
            const OPTIONS: [UiFont; 3] = [UiFont::Small, UiFont::Standard, UiFont::Large];
            let current = OPTIONS
                .iter()
                .position(|&f| f == self.cfg.ui_font)
                .unwrap_or(1);
            let next = (current as isize + delta).rem_euclid(OPTIONS.len() as isize) as usize;
            self.cfg.ui_font = OPTIONS[next];
            self.clear_notice();
        }

        /// 主题色分段组:左右键在预设间循环。
        fn accent_shift(&mut self, delta: isize) {
            let n = theme::ACCENTS.len() as isize;
            let next = (self.cfg.ui_accent as isize + delta).rem_euclid(n) as usize;
            self.cfg.ui_accent = next;
            self.clear_notice();
        }

        /// 更新渠道分段组:左右键在稳定/测试之间切换。
        fn channel_shift(&mut self, delta: isize) {
            let current = if self.cfg.update_channel == Channel::Beta { 1 } else { 0 };
            let next = (current + delta).rem_euclid(2);
            self.cfg.update_channel = Channel::from_index(next);
            self.clear_notice();
        }

        /// Enter / Space 激活键盘焦点控件;与对应鼠标点击走同一套动作。
        /// 段落组用左右键改值,这里不响应。
        fn activate(&mut self, k: Kbd, cx: &mut Context<Self>) {
            match k {
                Kbd::Update => match self.updates.clone() {
                    UpdateState::Available(release) => self.install_update(release, cx),
                    UpdateState::Checking => {}
                    UpdateState::Latest | UpdateState::NoRelease | UpdateState::Failed(_) => {
                        self.check_updates(cx);
                    }
                },
                Kbd::Scheme => {}
                Kbd::Aux => {
                    self.cfg.aux = !self.cfg.aux;
                    self.clear_notice();
                }
                Kbd::ChinesePunct => {
                    self.cfg.chinese_punct = !self.cfg.chinese_punct;
                    self.clear_notice();
                }
                Kbd::ShiftToggle => {
                    self.cfg.shift_toggle = !self.cfg.shift_toggle;
                    self.clear_notice();
                }
                Kbd::Traditional => {
                    self.cfg.traditional = !self.cfg.traditional;
                    self.clear_notice();
                }
                Kbd::Region | Kbd::PageSize | Kbd::Layout | Kbd::Font | Kbd::Accent
                | Kbd::Channel => {}
                Kbd::Effects => self.open_effects(),
                Kbd::EffectsToggle => {
                    self.cfg.effects = !self.cfg.effects;
                    self.clear_notice();
                }
                Kbd::Prediction => {
                    self.cfg.predict = !self.cfg.predict;
                    self.clear_notice();
                }
                Kbd::ResetFuzzy => {
                    if self.groups.iter().any(|(_, count)| *count > 0) {
                        match call(&Request::ResetFuzzy) {
                            Some(reply) => {
                                self.groups = reply.groups;
                                self.say(Notice::Success, "已重置模糊音组统计");
                            }
                            None => self.say(Notice::Error, "输入法服务未运行,启动后再试"),
                        }
                    }
                }
                Kbd::Fuzzy => {
                    self.cfg.fuzzy = if self.cfg.fuzzy.is_off() {
                        Fuzzy::auto()
                    } else {
                        Fuzzy::off()
                    };
                    self.clear_notice();
                }
                Kbd::ImportUser => self.import_user(),
                Kbd::ExportUser => self.export_user(),
                Kbd::Phrases => self.open_phrases(),
                Kbd::PunctFile => self.open_punct(),
                Kbd::AppsFile => self.open_apps(),
                Kbd::BackupExport => self.backup_export(),
                Kbd::BackupImport => self.backup_import(),
                Kbd::Diagnostics => self.export_diagnostics(),
                Kbd::ClearUser => {
                    if self.confirm_clear {
                        self.confirm_clear = false;
                        if send(&Request::ClearUser) {
                            self.say(Notice::Success, "已清空用户词库");
                        } else {
                            self.say(Notice::Error, "输入法服务未运行,启动后再试");
                        }
                    } else {
                        self.confirm_clear = true;
                        self.say(Notice::Info, "再点一次确认清空");
                    }
                }
                Kbd::Close => cx.quit(),
                Kbd::Save => self.save(),
            }
            cx.notify();
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

        /// 版本、配置、模糊组统计与服务日志末尾拼成一份报告,贴给排障用。
        fn export_diagnostics(&mut self) {
            self.confirm_clear = false;
            let Some(path) = choose_diag_file() else {
                return;
            };
            let log = config::log_path().and_then(|path| std::fs::read(path).ok());
            let (note, tail) = match &log {
                Some(bytes) => ("", tail_text(bytes, 64 * 1024)),
                None => ("(没有找到服务日志)\n", String::new()),
            };
            let groups = self
                .groups
                .iter()
                .filter(|(_, count)| *count > 0)
                .map(|(name, count)| format!("{name} {count}"))
                .collect::<Vec<_>>();
            let status = call(&Request::Status).and_then(|reply| reply.status);
            let server_version = status
                .as_ref()
                .map(|s| s.server_version.clone())
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
            let report = format!(
                "掠影诊断报告\n\
                 生成时间: {}\n\
                 设置窗版本: {}\n\
                 服务版本: {}\n\
                 资源: {}\n\
                 系统: {} / {}\n\
                 模糊组: {}\n\
                 配置: {:?}\n\n\
                 --- 服务日志(末尾 64 KB)---\n\
                 {note}{tail}\n",
                local_stamp(),
                self.current,
                server_version,
                status_detail(&status),
                std::env::consts::OS,
                std::env::consts::ARCH,
                if groups.is_empty() {
                    "(无)".to_string()
                } else {
                    groups.join(", ")
                },
                self.cfg,
            );
            match std::fs::write(&path, report) {
                Ok(()) => self.say(Notice::Success, format!("已导出到 {}", path.display())),
                Err(e) => self.say(Notice::Error, format!("导出失败: {e}")),
            }
        }

        fn open_phrases(&mut self) {
            self.confirm_clear = false;
            self.open_text_file(
                config::phrases_path(),
                lueying_features::phrases::TEMPLATE,
                "没有可写的短语文件路径",
            );
        }

        fn open_punct(&mut self) {
            self.confirm_clear = false;
            self.open_text_file(config::punct_path(), config::PUNCT_TEMPLATE, "没有可写的标点文件路径");
        }

        fn open_apps(&mut self) {
            self.confirm_clear = false;
            self.open_text_file(config::apps_path(), config::APPS_TEMPLATE, "没有可写的应用配置路径");
        }

        /// 打开用户效果目录:先让常驻服务给出三个槽位的模板(缺文件才写),
        /// 再拉起资源管理器。模板是起点,用户改完存盘即生效。
        fn open_effects(&mut self) {
            self.confirm_clear = false;
            let Some(dir) = config::effects_dir() else {
                self.say(Notice::Error, "没有可写的效果目录");
                return;
            };
            let Some(reply) = call(&Request::EffectTemplates) else {
                self.say(Notice::Error, "输入法服务未运行,启动后再试");
                return;
            };
            if let Err(e) = std::fs::create_dir_all(&dir) {
                self.say(Notice::Error, format!("建目录失败: {e}"));
                return;
            }
            let mut written = 0;
            for (name, source) in &reply.templates {
                let path = dir.join(name);
                if path.exists() {
                    continue;
                }
                match std::fs::write(&path, source) {
                    Ok(()) => written += 1,
                    Err(e) => {
                        self.say(Notice::Error, format!("写 {} 失败: {e}", path.display()));
                        return;
                    }
                }
            }
            match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
                Ok(_) => self.say(
                    Notice::Info,
                    if written > 0 {
                        format!("已写入 {written} 个模板,打开 {}", dir.display())
                    } else {
                        format!("已打开 {}", dir.display())
                    },
                ),
                Err(e) => self.say(Notice::Error, format!("打开失败: {e}")),
            }
        }

        /// 打开一份用户数据文件;缺失时先写模板。保存后由 Server 在下次
        /// 重载配置时读回。
        fn open_text_file(&mut self, path: Option<PathBuf>, template: &str, empty: &str) {
            let Some(path) = path else {
                self.say(Notice::Error, empty);
                return;
            };
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if !path.exists() {
                let _ = std::fs::write(&path, template);
            }
            match std::process::Command::new("notepad.exe").arg(&path).spawn() {
                Ok(_) => self.say(Notice::Info, format!("已打开 {}", path.display())),
                Err(e) => self.say(Notice::Error, format!("打开失败: {e}")),
            }
        }

        /// 导出配置备份:配置、短语、标点、按应用配置与用户词库打包成
        /// 一份文本。文件内容按行数声明长度,文件里出现什么标记都不影响
        /// 解析;服务未运行时词库一块跳过。
        fn backup_export(&mut self) {
            self.confirm_clear = false;
            let stamp = local_stamp().replace([':', ' '], "-");
            let Some(path) = choose_file(
                true,
                &format!("lueying-backup-{stamp}.txt"),
                "导出配置备份",
                "备份文件 (*.txt)\0*.txt\0所有文件 (*.*)\0*.*\0\0",
                "txt",
            ) else {
                return;
            };
            let mut files: Vec<(String, String)> = Vec::new();
            for (name, path) in [
                ("config.ini", config::path()),
                ("phrases.txt", config::phrases_path()),
                ("punct.txt", config::punct_path()),
                ("apps.ini", config::apps_path()),
            ] {
                if let Some(path) = path
                    && let Ok(text) = std::fs::read_to_string(&path)
                {
                    files.push((name.to_string(), text));
                }
            }
            if let Some(reply) = call(&Request::ExportUser) {
                files.push(("user.tsv".to_string(), reply.user));
            }
            if files.is_empty() {
                self.say(Notice::Error, "没有可备份的数据");
                return;
            }
            let mut out = String::from("### lueying-backup v1\n");
            for (name, content) in &files {
                let lines = content.lines().count();
                out.push_str(&format!("### begin: {name} {lines}\n"));
                if lines > 0 {
                    out.push_str(content.trim_end_matches('\n'));
                    out.push('\n');
                }
                out.push_str(&format!("### end: {name}\n"));
            }
            match std::fs::write(&path, out) {
                Ok(()) => self.say(Notice::Success, format!("已备份到 {}", path.display())),
                Err(e) => self.say(Notice::Error, format!("导出失败: {e}")),
            }
        }

        /// 导入配置备份:逐块写回对应文件,用户词库走管道合并;最后让
        /// Server 重读配置并刷新设置窗里的当前值。
        fn backup_import(&mut self) {
            self.confirm_clear = false;
            let Some(path) = choose_file(
                false,
                "",
                "导入配置备份",
                "备份文件 (*.txt)\0*.txt\0所有文件 (*.*)\0*.*\0\0",
                "txt",
            ) else {
                return;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => {
                    self.say(Notice::Error, format!("读取失败: {e}"));
                    return;
                }
            };
            if !text.starts_with("### lueying-backup v1") {
                self.say(Notice::Error, "这份文件不是掠影备份");
                return;
            }
            let files = unpack_backup(&text);
            let mut written = 0usize;
            let mut imported_user = false;
            for (name, content) in &files {
                let target = match name.as_str() {
                    "config.ini" => config::path(),
                    "phrases.txt" => config::phrases_path(),
                    "punct.txt" => config::punct_path(),
                    "apps.ini" => config::apps_path(),
                    "user.tsv" => {
                        if send(&Request::ImportUser { text: content.clone() }) {
                            imported_user = true;
                        }
                        continue;
                    }
                    _ => continue,
                };
                if let Some(path) = target {
                    let body = if content.is_empty() {
                        String::new()
                    } else {
                        format!("{content}\n")
                    };
                    if std::fs::write(&path, body).is_ok() {
                        written += 1;
                    }
                }
            }
            reload();
            if let Some(path) = &self.path {
                self.cfg = config::load(path);
            }
            if written > 0 || imported_user {
                self.say(
                    Notice::Success,
                    format!("已恢复 {written} 份配置文件{}", if imported_user { "与词库" } else { "" }),
                );
            } else {
                self.say(Notice::Error, "备份里没有可恢复的内容");
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

        /// 后台查 GitHub Release;网络慢不能卡住设置窗。渠道跟随当前配置。
        fn check_updates(&mut self, cx: &mut Context<Self>) {
            self.clear_notice();
            self.updates = UpdateState::Checking;
            let channel = self.cfg.update_channel;
            let task = cx
                .background_executor()
                .spawn(async move { update::check_channel(channel) });
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
        choose_file(
            save,
            if save { "lueying-user.tsv" } else { "" },
            if save {
                "导出用户词库"
            } else {
                "导入用户词库"
            },
            "用户词库 (*.tsv)\0*.tsv\0所有文件 (*.*)\0*.*\0\0",
            "tsv",
        )
    }

    /// 诊断报告的另存对话框。
    fn choose_diag_file() -> Option<PathBuf> {
        choose_file(
            true,
            "lueying-diag.txt",
            "导出诊断",
            "文本文件 (*.txt)\0*.txt\0所有文件 (*.*)\0*.*\0\0",
            "txt",
        )
    }

    /// 解析备份文件:`### begin: <名字> <行数>` 之后逐行取声明数量的行,
    /// 随后跳过 `### end:` 行;坏块整块跳过。
    fn unpack_backup(text: &str) -> Vec<(String, String)> {
        let mut files = Vec::new();
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.next() {
            let Some(rest) = line.strip_prefix("### begin: ") else {
                continue;
            };
            let Some((name, count)) = rest.rsplit_once(' ') else {
                continue;
            };
            let Ok(count) = count.parse::<usize>() else {
                continue;
            };
            let mut content = Vec::with_capacity(count);
            for _ in 0..count {
                match lines.next() {
                    Some(line) => content.push(line.to_string()),
                    None => break,
                }
            }
            lines.next_if(|line| line.starts_with("### end:"));
            files.push((name.to_string(), content.join("\n")));
        }
        files
    }

    /// `filter` 自带双 NUL 结尾;标题与扩展名由这里补结尾 NUL。
    fn choose_file(
        save: bool,
        name: &str,
        title: &str,
        filter: &str,
        extension: &str,
    ) -> Option<PathBuf> {
        let mut buf = [0u16; 1024];
        for (slot, ch) in buf.iter_mut().zip(name.encode_utf16()) {
            *slot = ch;
        }
        let filter: Vec<u16> = filter.encode_utf16().collect();
        let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let extension: Vec<u16> = extension.encode_utf16().chain(std::iter::once(0)).collect();
        let mut dialog = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
            lpstrFilter: PCWSTR(filter.as_ptr()),
            lpstrFile: PWSTR(buf.as_mut_ptr()),
            nMaxFile: buf.len() as u32,
            lpstrTitle: PCWSTR(title.as_ptr()),
            Flags: OFN_PATHMUSTEXIST
                | if save {
                    OFN_OVERWRITEPROMPT
                } else {
                    OFN_FILEMUSTEXIST
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

    /// 诊断报告抬头的本地时间。
    fn local_stamp() -> String {
        let st = unsafe { GetLocalTime() };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        )
    }

    /// 日志只取末尾一段;截断处可能切在字符中间,按替换字符解码。
    fn tail_text(bytes: &[u8], limit: usize) -> String {
        let start = bytes.len().saturating_sub(limit);
        String::from_utf8_lossy(&bytes[start..]).into_owned()
    }

    /// 「关于」页的状态行:词典来源与规模、模型是否加载。
    fn status_detail(status: &Option<EngineStatus>) -> String {
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

    fn notice_color(notice: Notice, colors: theme::Colors) -> u32 {
        match notice {
            Notice::Success => colors.success,
            Notice::Error => colors.danger,
            Notice::None | Notice::Info => colors.text_secondary,
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
            // 效果目录:用户在设置窗里按「目录」直接打开,文件放进去即生效。
            let effects_detail = match config::effects_dir() {
                Some(dir) => format!("SkSL 装饰层,{}", dir.display()),
                None => "SkSL 装饰层,没有可写的目录".to_string(),
            };

            let scheme_control = div()
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
                .when(self.kbd_is(Kbd::Scheme), |el| {
                    el.border_2().border_color(rgb(colors.accent))
                })
                .children(SchemeId::ALL.into_iter().enumerate().map(|(index, id)| {
                    segment(
                        "scheme",
                        index,
                        id.name(),
                        44.0,
                        self.cfg.scheme == id,
                        high_contrast,
                        colors,
                        cx.listener(move |this, _, _, cx| {
                            this.cfg.scheme = id;
                            this.clear_notice();
                            cx.notify();
                        }),
                    )
                }));

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
                // 键盘焦点落在分段组时高亮外框,左右键改值。
                .when(self.kbd_is(Kbd::PageSize), |el| {
                    el.border_2().border_color(rgb(colors.accent))
                })
                .children([3usize, 5, 9].into_iter().map(|n| {
                    segment(
                        "page-size",
                        n,
                        n.to_string(),
                        34.0,
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

            // 繁体字形地区;只在开着繁体时出现,值为 region 在选项表里的下标。
            let region_control = div()
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
                .when(self.kbd_is(Kbd::Region), |el| {
                    el.border_2().border_color(rgb(colors.accent))
                })
                .children(Region::NAMES.into_iter().enumerate().map(|(index, name)| {
                    segment(
                        "region",
                        index,
                        name,
                        44.0,
                        self.cfg.region as usize == index,
                        high_contrast,
                        colors,
                        cx.listener(move |this, _, _, cx| {
                            this.cfg.region = Region::from_index(index);
                            this.clear_notice();
                            cx.notify();
                        }),
                    )
                }));

            // 候选排列:横排一行自动换行,竖排一行一条。
            let layout_control = div()
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
                .when(self.kbd_is(Kbd::Layout), |el| {
                    el.border_2().border_color(rgb(colors.accent))
                })
                .children(UiLayout::NAMES.into_iter().enumerate().map(|(index, name)| {
                    segment(
                        "layout",
                        index,
                        name,
                        44.0,
                        (self.cfg.ui_layout == UiLayout::Vertical) as usize == index,
                        high_contrast,
                        colors,
                        cx.listener(move |this, _, _, cx| {
                            this.cfg.ui_layout = UiLayout::from_index(index);
                            this.clear_notice();
                            cx.notify();
                        }),
                    )
                }));

            // 候选字号:三档缩放,拼音行不受影响。
            let font_options = [UiFont::Small, UiFont::Standard, UiFont::Large];
            let font_control = div()
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
                .when(self.kbd_is(Kbd::Font), |el| {
                    el.border_2().border_color(rgb(colors.accent))
                })
                .children(UiFont::NAMES.into_iter().enumerate().map(|(index, name)| {
                    segment(
                        "font",
                        index,
                        name,
                        44.0,
                        font_options[index] == self.cfg.ui_font,
                        high_contrast,
                        colors,
                        cx.listener(move |this, _, _, cx| {
                            this.cfg.ui_font = font_options[index];
                            this.clear_notice();
                            cx.notify();
                        }),
                    )
                }));

            // 主题色:预设名做分段,选中格文字换成对应颜色,所见即所得。
            let accent_index = self.cfg.ui_accent.min(theme::ACCENTS.len() - 1);
            let accent_control = div()
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
                .when(self.kbd_is(Kbd::Accent), |el| {
                    el.border_2().border_color(rgb(colors.accent))
                })
                .children(theme::ACCENTS.iter().enumerate().map(|(index, accent)| {
                    let color = match appearance {
                        theme::Appearance::Dark => accent.dark[0],
                        _ => accent.light[0],
                    };
                    div()
                        .id(("accent", index))
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(34.0))
                        .h(px(26.0))
                        .rounded_md()
                        .text_size(px(12.0))
                        .cursor_pointer()
                        .when(index == accent_index, |el| {
                            el.bg(rgb(colors.segment))
                                .text_color(rgb(color))
                                .font_weight(FontWeight::SEMIBOLD)
                        })
                        .when(index != accent_index, |el| {
                            el.text_color(rgb(colors.text_secondary))
                                .hover(|style| style.text_color(rgb(colors.text)))
                        })
                        .active(|style| style.opacity(0.8))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cfg.ui_accent = index;
                            this.clear_notice();
                            cx.notify();
                        }))
                        .child(accent.name)
                }));

            // 更新渠道:稳定只认正式版,测试跟进预发布。
            let channel_control = div()
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
                .when(self.kbd_is(Kbd::Channel), |el| {
                    el.border_2().border_color(rgb(colors.accent))
                })
                .children(Channel::NAMES.into_iter().enumerate().map(|(index, name)| {
                    segment(
                        "channel",
                        index,
                        name,
                        44.0,
                        (self.cfg.update_channel == Channel::Beta) as usize == index,
                        high_contrast,
                        colors,
                        cx.listener(move |this, _, _, cx| {
                            this.cfg.update_channel = Channel::from_index(index);
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
                    "输入方案",
                    "全拼或双拼,共享词库",
                    scheme_control,
                    colors,
                    false,
                ))
                .child(setting_row(
                    "辅助码",
                    "拼音后按 ` 再打 1–2 位五笔码",
                    toggle(
                        "aux-code",
                        self.cfg.aux,
                        self.kbd_is(Kbd::Aux),
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.cfg.aux = !this.cfg.aux;
                            this.clear_notice();
                            cx.notify();
                        }),
                    ),
                    colors,
                    false,
                ))
                .child(setting_row(
                    "中文标点",
                    "按中文习惯输出 ，。；：“”",
                    toggle(
                        "chinese-punct",
                        self.cfg.chinese_punct,
                        self.kbd_is(Kbd::ChinesePunct),
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
                        self.kbd_is(Kbd::ShiftToggle),
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
                        self.kbd_is(Kbd::Traditional),
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
                // 字形地区常驻显示:关着繁体时是下次开启的预选项,
                // 免得窗口高度随开关跳动。通用是 OpenCC 标准,台湾/香港
                // 在其上换异体字,不做词汇替换。
                .child(setting_row(
                    "字形地区",
                    "台湾、香港按当地常用字形",
                    region_control,
                    colors,
                    false,
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
                    "候选排列",
                    "横排挤一行,竖排一行一条",
                    layout_control,
                    colors,
                    false,
                ))
                .child(setting_row(
                    "候选字号",
                    "只缩放候选行,拼音行不变",
                    font_control,
                    colors,
                    false,
                ))
                .child(setting_row(
                    "主题色",
                    "候选窗的强调色,高对比度下跟随系统",
                    accent_control,
                    colors,
                    false,
                ))
                .child(setting_row(
                    "用户效果",
                    effects_detail,
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(button(
                            "open-effects",
                            "目录",
                            ButtonKind::Secondary,
                            true,
                            self.kbd_is(Kbd::Effects),
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.open_effects();
                                cx.notify();
                            }),
                        ))
                        .child(toggle(
                            "effects",
                            self.cfg.effects,
                            self.kbd_is(Kbd::EffectsToggle),
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.cfg.effects = !this.cfg.effects;
                                this.clear_notice();
                                cx.notify();
                            }),
                        )),
                    colors,
                    false,
                ))
                .child(setting_row(
                    "下一词联想",
                    "按本机上屏词序补全",
                    toggle(
                        "prediction",
                        self.cfg.predict,
                        self.kbd_is(Kbd::Prediction),
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.cfg.predict = !this.cfg.predict;
                            this.clear_notice();
                            cx.notify();
                        }),
                    ),
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
                            self.kbd_is(Kbd::ResetFuzzy),
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
                            self.kbd_is(Kbd::Fuzzy),
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
                    "自定义短语",
                    "编辑 phrases.txt，保存后重载",
                    button(
                        "open-phrases",
                        "编辑",
                        ButtonKind::Secondary,
                        true,
                        self.kbd_is(Kbd::Phrases),
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.open_phrases();
                            cx.notify();
                        }),
                    ),
                    colors,
                    false,
                ))
                .child(setting_row(
                    "自定义标点",
                    "编辑 punct.txt，映射半角符号的输出",
                    button(
                        "open-punct",
                        "编辑",
                        ButtonKind::Secondary,
                        true,
                        self.kbd_is(Kbd::PunctFile),
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.open_punct();
                            cx.notify();
                        }),
                    ),
                    colors,
                    false,
                ))
                .child(setting_row(
                    "按应用配置",
                    "编辑 apps.ini，单个应用关标点或学习",
                    button(
                        "open-apps",
                        "编辑",
                        ButtonKind::Secondary,
                        true,
                        self.kbd_is(Kbd::AppsFile),
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.open_apps();
                            cx.notify();
                        }),
                    ),
                    colors,
                    false,
                ))
                .child(setting_row(
                    "配置备份",
                    "配置、短语、标点、按应用与词库打包成一份文本",
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(button(
                            "backup-export",
                            "导出",
                            ButtonKind::Secondary,
                            true,
                            self.kbd_is(Kbd::BackupExport),
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.backup_export();
                                cx.notify();
                            }),
                        ))
                        .child(button(
                            "backup-import",
                            "导入",
                            ButtonKind::Secondary,
                            true,
                            self.kbd_is(Kbd::BackupImport),
                            colors,
                            cx.listener(|this, _, _, cx| {
                                this.backup_import();
                                cx.notify();
                            }),
                        )),
                    colors,
                    false,
                ))
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
                            self.kbd_is(Kbd::ImportUser),
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
                            self.kbd_is(Kbd::ExportUser),
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
                            self.kbd_is(Kbd::ClearUser),
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
                ))
                .child(section_label("关于", colors, false))
                .child(setting_row(
                    "引擎状态",
                    status_detail(&self.status),
                    div(),
                    colors,
                    false,
                ))
                .child(setting_row(
                    "更新渠道",
                    "测试版会跟进预发布 Release",
                    channel_control,
                    colors,
                    false,
                ))
                .child(setting_row(
                    "诊断信息",
                    "版本、配置与服务日志导出成一份文本",
                    button(
                        "diagnostics",
                        "导出",
                        ButtonKind::Secondary,
                        true,
                        self.kbd_is(Kbd::Diagnostics),
                        colors,
                        cx.listener(|this, _, _, cx| {
                            this.export_diagnostics();
                            cx.notify();
                        }),
                    ),
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
                    self.kbd_is(Kbd::Update),
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
                            self.kbd_is(Kbd::Close),
                            colors,
                            cx.listener(|_, _, _, cx| cx.quit()),
                        ))
                        .child(button(
                            "save",
                            "保存",
                            ButtonKind::Primary,
                            true,
                            self.kbd_is(Kbd::Save),
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
                .gap(px(10.0))
                .p(px(18.0))
                .w(px(WIDTH))
                .h(px(self.height))
                .bg(rgb(colors.bg))
                .text_color(rgb(colors.text))
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    let key = event.keystroke.key.as_str();
                    match key {
                        "escape" => cx.quit(),
                        "s" if event.keystroke.modifiers.control => {
                            this.save();
                            cx.notify();
                        }
                        "tab" => {
                            this.kbd_step(if event.keystroke.modifiers.shift {
                                -1
                            } else {
                                1
                            });
                            cx.notify();
                        }
                        "enter" | "space" => {
                            if let Some(i) = this.kbd {
                                this.activate(KBD_ORDER[i], cx);
                            }
                        }
                        "left" | "right" => {
                            let delta = if key == "left" { -1 } else { 1 };
                            match this.kbd.map(|i| KBD_ORDER[i]) {
                                Some(Kbd::Scheme) => {
                                    this.scheme_shift(delta);
                                    cx.notify();
                                }
                                Some(Kbd::Region) => {
                                    this.region_shift(delta);
                                    cx.notify();
                                }
                                Some(Kbd::PageSize) => {
                                    this.page_shift(delta);
                                    cx.notify();
                                }
                                Some(Kbd::Layout) => {
                                    this.layout_shift(delta);
                                    cx.notify();
                                }
                                Some(Kbd::Font) => {
                                    this.font_shift(delta);
                                    cx.notify();
                                }
                                Some(Kbd::Accent) => {
                                    this.accent_shift(delta);
                                    cx.notify();
                                }
                                Some(Kbd::Channel) => {
                                    this.channel_shift(delta);
                                    cx.notify();
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }))
                .child(header)
                .child(
                    div()
                        .id("settings-scroll")
                        .flex_1()
                        .min_h(px(0.0))
                        .w_full()
                        .overflow_y_scroll()
                        .overflow_x_hidden()
                        .child(card),
                )
                .child(footer)
                .into_any_element()
        }
    }
}
