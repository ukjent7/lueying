//! 设置窗:Slint(winit 后端 + femtovg GPU 渲染,软件渲染兜底)承载页面,视觉对齐 macOS 27
//! 系统设置。页面模型在 `lueying_server::settings_view` 与
//! `settings_model`,平台侧动作在 `settings_actions`;这里只做三件事:
//! 把模型灌进 Slint 属性、把回调翻译回 `Settings::activate`、把要碰
//! 文件系统与网络的动作放到后台线程再回事件循环。
//!
//! `--selftest <报告路径>` 跑一遍自检:模型五页铺得开、关键控件在位、
//! Slint 组件能实例化并吃进页面数据。Windows CI 用它确认设置窗链路没断。

use std::path::Path;
use std::sync::{Arc, Mutex};

use lueying_net::update::{self, Check, Release};
use lueying_protocol::Request;

use lueying_server::accents::ACCENTS;
use lueying_server::config::{self, AppConfig};
use lueying_server::settings_actions;
use lueying_server::settings_model::{self as model, ButtonKind, Control};
use lueying_server::settings_official;
use lueying_server::settings_secret;
use lueying_server::settings_view::{
    Action, ConsentState, Notice, PageColors, PageId, Settings, UpdateState,
};
use lueying_server::theme;

slint::include_modules!();

/// Vec 灌进 Slint 属性的统一入口。
fn items<T: 'static + Clone>(vec: Vec<T>) -> slint::ModelRc<T> {
    slint::ModelRc::new(slint::VecModel::from(vec))
}

/// 窗口状态:配置与平台侧数据;回调与后台线程共享。
struct Core {
    settings: Settings,
    /// 待安装的新版;检查更新成功后填入。
    release: Option<Release>,
}

type Shared = Arc<Mutex<Core>>;

/// 打开设置窗并跑事件循环;窗口关掉才返回。
pub fn run() {
    let path = config::path();
    let cfg = path
        .as_ref()
        .map_or_else(AppConfig::default, |path| config::load(path));
    let mut settings = Settings::with_config(cfg);
    if let Some(issue) = settings.cfg.config_issues.first() {
        let message = format!(
            "配置有 {} 处问题:{} ({})",
            settings.cfg.config_issues.len(),
            issue.message,
            issue.layer
        );
        settings.say(Notice::Error, message);
    }
    settings.path = path;
    settings.groups = settings_actions::call(&Request::FuzzyStats)
        .map(|reply| reply.groups)
        .unwrap_or_default();
    settings.status = settings_actions::call(&Request::Status).and_then(|reply| reply.status);
    settings.current = update::current_version();
    settings.scripts = settings_actions::installed_scripts();
    settings.ppocr_ready = settings_actions::ppocr_ready();
    settings.ext_metrics = settings_actions::read_ext_metrics();

    let core: Shared = Arc::new(Mutex::new(Core {
        settings,
        release: None,
    }));
    let ui = SettingsWindow::new().expect("Slint 设置窗创建失败");
    refresh(&ui, &core);

    let handle = ui.as_weak();
    let shared = Arc::clone(&core);
    ui.on_activate(move |base, index| {
        let Some(ui) = handle.upgrade() else {
            return;
        };
        let id = model::Id::new(base.to_string(), index.max(0) as usize);
        let action = {
            let mut guard = lock(&shared);
            let switched = PageId::from_nav(&id.base).is_some();
            let action = guard.settings.activate(&id);
            if switched {
                guard.settings.clear_notice();
            }
            action
        };
        if let Some(action) = action {
            dispatch(&ui, &shared, action);
        }
        refresh(&ui, &shared);
    });
    let handle = ui.as_weak();
    let shared = Arc::clone(&core);
    ui.on_search_edited(move || {
        let Some(ui) = handle.upgrade() else {
            return;
        };
        let text = ui.get_search_text().to_string();
        {
            let mut guard = lock(&shared);
            guard.settings.set_search(text);
        }
        refresh(&ui, &shared);
    });
    let shared_close = Arc::clone(&core);
    ui.on_save_hotkey(move || {
        let mut guard = lock(&shared_close);
        settings_actions::save(&mut guard.settings);
    });
    let handle = ui.as_weak();
    ui.on_escape(move || {
        if let Some(ui) = handle.upgrade() {
            let _ = ui.hide();
        }
    });
    ui.run().expect("Slint 事件循环退出失败");
}

/// 取锁;毒锁当普通锁用,设置窗不值得为一个互斥量崩掉。
fn lock(core: &Shared) -> std::sync::MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// 执行动作。开子窗与后台线程的动作单独处理,其余改完状态即回。
fn dispatch(ui: &SettingsWindow, core: &Shared, action: Action) {
    match action {
        Action::Close => {
            let _ = ui.hide();
        }
        Action::Save => with(core, settings_actions::save),
        Action::ResetFuzzy => with(core, settings_actions::reset_fuzzy),
        Action::OpenScripts => with(core, settings_actions::open_scripts),
        Action::ImportLuext => with(core, settings_actions::import_luext),
        Action::ApproveExtension => with(core, settings_actions::approve_extension),
        Action::OpenExtAudit => with(core, settings_actions::open_ext_audit),
        Action::RefreshExtMetrics => with(core, settings_actions::refresh_ext_metrics),
        Action::SceneDebug => with(core, settings_actions::refresh_scene_debug),
        Action::OpenOcrModels => with(core, settings_actions::open_ocr_models),
        Action::OpenApps => with(core, settings_actions::open_apps),
        Action::BackupExport => with(core, settings_actions::backup_export),
        Action::BackupImport => with(core, settings_actions::backup_import),
        Action::ImportUser => with(core, settings_actions::import_user),
        Action::ExportUser => with(core, settings_actions::export_user),
        Action::Diagnostics => with(core, settings_actions::export_diagnostics),
        Action::ClearUser => {
            let mut guard = lock(core);
            if guard.settings.confirm_clear {
                settings_actions::clear_user(&mut guard.settings);
            } else {
                guard.settings.confirm_clear = true;
                guard.settings.say(Notice::Info, "再点一次确认清空");
            }
        }
        Action::ChooseScript => choose_script(ui, core),
        Action::AddApp => add_app(ui, core),
        Action::EditSecret(ext, key, title) => open_secret_editor(ui, core, ext, key, title),
        Action::OpenPhrases => open_text_editor(ui, core, TextKind::Phrases),
        Action::OpenPunct => open_text_editor(ui, core, TextKind::Punct),
        Action::CheckUpdate => start_update_check(ui, core),
        Action::InstallUpdate(_) => install_update(ui, core),
        Action::InstallOfficial(id) => start_official_install(ui, core, id),
    }
}

/// 取出状态、跑动作;调用方随后 refresh。
fn with(core: &Shared, f: impl FnOnce(&mut Settings)) {
    let mut guard = lock(core);
    f(&mut guard.settings);
}

/// 把当前状态灌进窗口:配色全局、页面模型与页脚提示。
fn refresh(ui: &SettingsWindow, core: &Shared) {
    let appearance = theme::Appearance::of_system();
    let guard = lock(core);
    let accent = guard.settings.cfg.ui_accent.min(ACCENTS.len() - 1);
    let colors = theme::colors_with(appearance, accent);
    let theme_colors = ui.global::<Theme>();
    theme_colors.set_accent(color(colors.accent));
    theme_colors.set_on_accent(color(colors.on_accent));
    theme_colors.set_accent_text(color(colors.accent_text));
    theme_colors.set_window_bg(color(colors.bg));
    theme_colors.set_bg(color(colors.surface));
    theme_colors.set_surface_alt(color(colors.surface_alt));
    theme_colors.set_hover(color(colors.hover));
    theme_colors.set_border(color(colors.border));
    theme_colors.set_border_strong(color(colors.border_strong));
    theme_colors.set_text(color(colors.text));
    theme_colors.set_text_secondary(color(colors.text_secondary));
    theme_colors.set_text_tertiary(color(colors.text_tertiary));
    theme_colors.set_success(color(colors.success));
    theme_colors.set_danger(color(colors.danger));
    theme_colors.set_on_danger(color(colors.surface));

    let page_colors = page_colors(&colors, appearance);
    let page = guard.settings.page(&page_colors);
    ui.set_subtitle(page.subtitle.into());
    ui.set_search_text(page.search.into());
    ui.set_active_nav(page.active_nav as i32);
    ui.set_nav(items(
        page.nav
            .iter()
            .map(|item| NavEntry {
                base: item.id.base.clone().into(),
                index: item.id.index as i32,
                text: item.text.clone().into(),
            })
            .collect(),
    ));
    ui.set_groups(items(
        page.groups
            .iter()
            .map(|group| Group {
                title: group.title.clone().into(),
                rows: items(group.rows.iter().map(|row| slint_row(row)).collect()),
            })
            .collect(),
    ));
    ui.set_header_button(page.header_button.as_ref().map_or_else(
        || Btn {
            base: String::new().into(),
            index: 0,
            text: String::new().into(),
            kind: 2,
            enabled: false,
        },
        |button| slint_button(button),
    ));
    ui.set_notice(page.notice.into());
    ui.set_notice_kind(notice_kind(page.notice_tint, &page_colors));
    ui.set_footer_buttons(items(
        page.footer_buttons.iter().map(slint_button).collect(),
    ));
}

/// 提示颜色反查种类:0 info 1 success 2 error。
fn notice_kind(tint: u32, colors: &PageColors) -> i32 {
    if tint == colors.notice_success {
        1
    } else if tint == colors.notice_error {
        2
    } else {
        0
    }
}

fn slint_button(button: &model::Button) -> Btn {
    Btn {
        base: button.id.base.clone().into(),
        index: button.id.index as i32,
        text: button.text.clone().into(),
        kind: match button.kind {
            ButtonKind::Primary => 0,
            ButtonKind::Secondary => 1,
            ButtonKind::Ghost => 2,
            ButtonKind::Danger => 3,
        },
        enabled: button.enabled,
    }
}

fn slint_row(row: &model::Row) -> Row {
    let mut slint = Row {
        label: row.label.clone().into(),
        detail: row.detail.clone().into(),
        kind: 0,
        tinted: false,
        base: String::new().into(),
        index: 0,
        on: false,
        options: items(Vec::new()),
        selected: 0,
        buttons: items(Vec::new()),
        preview: Preview {
            accent: color(0x2f6fed),
            font_scale: 1.0,
            vertical: false,
            page_size: 5,
            pinyin: String::new().into(),
            candidates: items(Vec::new()),
        },
    };
    match &row.control {
        Control::Toggle { id, on } => {
            slint.kind = 1;
            slint.base = id.base.clone().into();
            slint.index = id.index as i32;
            slint.on = *on;
        }
        Control::Segment {
            id,
            options,
            selected,
            ..
        } => {
            slint.kind = 2;
            slint.base = id.base.clone().into();
            slint.index = id.index as i32;
            slint.selected = *selected as i32;
            slint.tinted =
                !options.is_empty() && options.iter().all(|option| option.tint.is_some());
            slint.options = items(
                options
                    .iter()
                    .map(|option| SegOpt {
                        text: option.text.clone().into(),
                        tint: color(option.tint.unwrap_or(0)),
                        has_tint: option.tint.is_some(),
                    })
                    .collect(),
            );
        }
        Control::Buttons { items: buttons } => {
            slint.kind = 3;
            slint.buttons = items(buttons.iter().map(slint_button).collect());
        }
        Control::ButtonsToggle {
            items: buttons,
            toggle,
            toggle_on,
        } => {
            slint.kind = 4;
            slint.base = toggle.base.clone().into();
            slint.index = toggle.index as i32;
            slint.on = *toggle_on;
            slint.buttons = items(buttons.iter().map(slint_button).collect());
        }
        Control::Preview { style, .. } => {
            slint.kind = 5;
            slint.preview = Preview {
                accent: color(style.accent),
                font_scale: style.font_scale,
                vertical: style.vertical,
                page_size: style.page_size as i32,
                pinyin: style.pinyin.clone().into(),
                candidates: items(
                    style
                        .candidates
                        .iter()
                        .map(String::as_str)
                        .map(Into::into)
                        .collect(),
                ),
            };
        }
        Control::None => {}
    }
    slint
}

/// 0xRRGGBB 转 Slint 颜色。
fn color(rgb: u32) -> slint::Color {
    slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// 页面里要用的颜色:提示色与主题色选项的着色。
fn page_colors(colors: &theme::Colors, appearance: theme::Appearance) -> PageColors {
    let accents = if matches!(appearance, theme::Appearance::HighContrast) {
        vec![colors.accent; ACCENTS.len()]
    } else {
        ACCENTS
            .iter()
            .map(|accent| match appearance {
                theme::Appearance::Dark => accent.dark[0],
                _ => accent.light[0],
            })
            .collect()
    };
    PageColors {
        notice_info: colors.text_secondary,
        notice_success: colors.success,
        notice_error: colors.danger,
        accents,
    }
}

/// 弹出扩展选择窗:关闭,或启用某个已安装的包;待同意的包先确认。
fn choose_script(ui: &SettingsWindow, core: &Shared) {
    let scripts = {
        let mut guard = lock(core);
        guard.settings.scripts = settings_actions::installed_scripts();
        if guard.settings.scripts.is_empty() {
            guard
                .settings
                .say(Notice::Info, "没有已安装的扩展;点「目录」放入扩展包");
            return;
        }
        guard.settings.scripts.clone()
    };
    let list = vec!["关闭运行期扩展".to_string()];
    let list = list
        .into_iter()
        .chain(
            scripts
                .iter()
                .map(|script| format!("{}  {}", script.name, script.id)),
        )
        .collect();
    open_picker(
        ui,
        core,
        "选择运行期扩展",
        list,
        move |_ui, core, index| {
            if index == 0 {
                let mut guard = lock(core);
                guard.settings.select_script(None);
                guard
                    .settings
                    .say(Notice::Info, "已关闭运行期扩展,保存后生效");
                return;
            }
            let Some(script) = scripts.get(index - 1) else {
                return;
            };
            let id = script.id.clone();
            let name = script.name.clone();
            // 确认框是模态泵,期间不能握着状态锁。
            let mut approved_now = false;
            if script.consent != ConsentState::Approved {
                let text = lueying_server::settings_consent::prompt(script);
                if !confirm(&text, "扩展授权") {
                    lock(core)
                        .settings
                        .say(Notice::Info, format!("未同意 {name},扩展保持停用"));
                    return;
                }
                if let Err(error) = settings_actions::approve_package(&id) {
                    lock(core)
                        .settings
                        .say(Notice::Error, format!("同意失败: {error}"));
                    return;
                }
                approved_now = true;
            }
            let mut guard = lock(core);
            if approved_now {
                guard.settings.scripts = settings_actions::installed_scripts();
            }
            if let Some(index) = guard.settings.scripts.iter().position(|item| item.id == id) {
                guard.settings.select_script(Some(index));
                guard
                    .settings
                    .say(Notice::Info, format!("已选择 {name},保存后生效"));
            }
        },
    );
}

/// 从正在运行的程序里挑一个加入按应用配置。
fn add_app(ui: &SettingsWindow, core: &Shared) {
    let apps = {
        let guard = lock(core);
        settings_actions::running_apps(&guard.settings.cfg.apps)
    };
    if apps.is_empty() {
        with(core, |settings| {
            settings.say(
                Notice::Info,
                "没有可添加的运行中程序;可打开「配置文件」手动添加",
            );
        });
        return;
    }
    let list = apps
        .iter()
        .map(|(exe, title)| format!("{title}  {exe}"))
        .collect();
    open_picker(ui, core, "添加应用", list, move |_ui, core, index| {
        let Some((exe, title)) = apps.get(index) else {
            return;
        };
        let mut guard = lock(core);
        guard
            .settings
            .cfg
            .apps
            .insert(exe.clone(), lueying_server::config::AppOverride::default());
        guard
            .settings
            .say(Notice::Info, format!("已添加 {title} ({exe});保存后生效"));
    });
}

/// 起一个单选弹出窗;挑选后回主线程改状态并刷新主窗。
fn open_picker(
    ui: &SettingsWindow,
    core: &Shared,
    title: &str,
    list: Vec<String>,
    picked: impl Fn(&SettingsWindow, &Shared, usize) + 'static,
) {
    let Ok(picker) = PickerWindow::new() else {
        return;
    };
    picker.set_win_title(title.into());
    picker.set_items(items(list.into_iter().map(Into::into).collect::<Vec<_>>()));
    let main = ui.as_weak();
    let shared = Arc::clone(core);
    let weak = picker.as_weak();
    picker.on_pick(move |index| {
        if let Some(picker) = weak.upgrade() {
            let _ = picker.hide();
        }
        if index >= 0
            && let Some(main) = main.upgrade()
        {
            picked(&main, &shared, index as usize);
            refresh(&main, &shared);
        }
    });
    let weak = picker.as_weak();
    picker.on_cancel(move || {
        if let Some(picker) = weak.upgrade() {
            let _ = picker.hide();
        }
    });
    let _ = picker.show();
}

/// 是/否确认框;Esc 或取消返回 false。
fn confirm(text: &str, title: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{IDOK, MB_ICONWARNING, MB_OKCANCEL, MessageBoxW};
    use windows::core::HSTRING;
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OKCANCEL | MB_ICONWARNING,
        ) == IDOK
    }
}

/// 扩展密钥编辑器:单行输入,存空串即删除。
fn open_secret_editor(ui: &SettingsWindow, core: &Shared, ext: String, key: String, title: String) {
    let Ok(editor) = SecretEditorWindow::new() else {
        return;
    };
    editor.set_win_title(format!("扩展密钥 · {title}").into());
    editor.set_hint("只保存在本机;留空并保存即删除".into());
    editor.set_body(settings_secret::get(&ext, &key).unwrap_or_default().into());
    let main = ui.as_weak();
    let shared = Arc::clone(core);
    let weak = editor.as_weak();
    editor.on_save(move || {
        let Some(editor) = weak.upgrade() else {
            return;
        };
        let text = editor.get_body().to_string();
        let _ = editor.hide();
        let mut guard = lock(&shared);
        match settings_secret::set(&ext, &key, &text) {
            Ok(()) => guard.settings.say(Notice::Success, "已保存扩展密钥"),
            Err(error) => guard
                .settings
                .say(Notice::Error, format!("保存失败: {error}")),
        }
        if let Some(main) = main.upgrade() {
            refresh(&main, &shared);
        }
    });
    let weak = editor.as_weak();
    editor.on_cancel(move || {
        if let Some(editor) = weak.upgrade() {
            let _ = editor.hide();
        }
    });
    let _ = editor.show();
}

/// 窗内文本编辑器要打开的文件:路径、标题与模板。
#[derive(Clone, Copy)]
enum TextKind {
    Phrases,
    Punct,
}

impl TextKind {
    fn path(self) -> Option<std::path::PathBuf> {
        match self {
            Self::Phrases => config::phrases_path(),
            Self::Punct => config::punct_path(),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Phrases => "编辑自定义短语",
            Self::Punct => "编辑自定义标点",
        }
    }

    fn template(self) -> &'static str {
        match self {
            Self::Phrases => lueying_features::phrases::TEMPLATE,
            Self::Punct => config::PUNCT_TEMPLATE,
        }
    }

    fn missing_path(self) -> &'static str {
        match self {
            Self::Phrases => "没有可写的短语文件路径",
            Self::Punct => "没有可写的标点文件路径",
        }
    }
}

/// 打开短语/标点编辑器:Slint 窗 + TextEdit,IME 由 winit 接管。
fn open_text_editor(ui: &SettingsWindow, core: &Shared, kind: TextKind) {
    let Some(path) = kind.path() else {
        with(core, |settings| {
            settings.say(Notice::Error, kind.missing_path());
        });
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if !path.exists()
        && let Err(e) = std::fs::write(&path, kind.template())
    {
        with(core, |settings| {
            settings.say(Notice::Error, format!("写模板失败: {e}"));
        });
        return;
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            with(core, |settings| {
                settings.say(Notice::Error, format!("读文件失败: {e}"));
            });
            return;
        }
    };
    let Ok(editor) = TextEditorWindow::new() else {
        return;
    };
    editor.set_win_title(kind.title().into());
    editor.set_body(text.into());
    let main = ui.as_weak();
    let shared = Arc::clone(core);
    let weak = editor.as_weak();
    editor.on_save(move || {
        let Some(editor) = weak.upgrade() else {
            return;
        };
        let body = editor.get_body().to_string();
        let _ = editor.hide();
        let mut guard = lock(&shared);
        match write_text(&path, &body) {
            Ok(()) => {
                settings_actions::reload();
                guard
                    .settings
                    .say(Notice::Success, format!("已保存{}", kind.title()));
            }
            Err(e) => guard
                .settings
                .say(Notice::Error, format!("写入 {} 失败: {e}", path.display())),
        }
        if let Some(main) = main.upgrade() {
            refresh(&main, &shared);
        }
    });
    let weak = editor.as_weak();
    editor.on_cancel(move || {
        if let Some(editor) = weak.upgrade() {
            let _ = editor.hide();
        }
    });
    let _ = editor.show();
}

/// 保存用户文本:临时文件、fsync、原子改名;断电不会留下半截文件。
fn write_text(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("txt.tmp");
    let mut file =
        std::fs::File::create(&tmp).map_err(|e| format!("创建 {}: {e}", tmp.display()))?;
    file.write_all(text.as_bytes())
        .map_err(|e| format!("写 {}: {e}", tmp.display()))?;
    file.sync_all()
        .map_err(|e| format!("同步 {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("改名 {}: {e}", path.display()))
}

/// 后台查更新:网络慢不能卡住窗口;结果回事件循环再改状态。
fn start_update_check(ui: &SettingsWindow, core: &Shared) {
    let channel = {
        let mut guard = lock(core);
        if matches!(guard.settings.updates, UpdateState::Checking) {
            return;
        }
        let channel = match guard.settings.cfg.update_channel {
            config::Channel::Beta => update::Channel::Beta,
            config::Channel::Stable => update::Channel::Stable,
        };
        guard.settings.updates = UpdateState::Checking;
        channel
    };
    refresh(ui, core);
    let handle = ui.as_weak();
    let shared = Arc::clone(core);
    let spawned = std::thread::Builder::new()
        .name("settings-update".into())
        .spawn(move || {
            let result = update::check_channel(channel);
            let _ = handle.upgrade_in_event_loop(move |ui| {
                let mut guard = lock(&shared);
                guard.settings.updates = match result {
                    Ok(Check::Latest) => UpdateState::Latest,
                    Ok(Check::Available(release)) => {
                        guard.release = Some(release.clone());
                        UpdateState::Available(release.version)
                    }
                    Ok(Check::NoRelease) => UpdateState::NoRelease,
                    Err(error) => UpdateState::Failed(error),
                };
                refresh(&ui, &shared);
            });
        });
    if let Err(e) = spawned {
        eprintln!("检查更新的线程启动失败: {e}");
    }
}

/// 确认后后台下载安装包并直接运行安装程序;设置窗保持可用。
fn install_update(ui: &SettingsWindow, core: &Shared) {
    let release = {
        let mut guard = lock(core);
        let Some(release) = guard.release.clone() else {
            return;
        };
        if !settings_actions::install_update(&mut guard.settings, &release) {
            return;
        }
        guard
            .settings
            .say(Notice::Info, "正在下载安装包;界面可以继续使用");
        release
    };
    refresh(ui, core);
    let handle = ui.as_weak();
    let shared = Arc::clone(core);
    let spawned = std::thread::Builder::new()
        .name("settings-update".into())
        .spawn(move || {
            let result = settings_actions::download_and_run_installer(&release);
            let _ = handle.upgrade_in_event_loop(move |ui| {
                let mut guard = lock(&shared);
                match result {
                    Ok(()) => guard
                        .settings
                        .say(Notice::Success, "安装程序已启动;按提示完成更新"),
                    Err(error) => guard
                        .settings
                        .say(Notice::Error, format!("更新失败: {error}")),
                }
                refresh(&ui, &shared);
            });
        });
    if let Err(error) = spawned {
        with(core, |settings| {
            settings.say(Notice::Error, format!("启动下载线程失败: {error}"));
        });
    }
}

/// 开始安装官方扩展/模型:先显示下载中,完成后回事件循环刷新。
fn start_official_install(ui: &SettingsWindow, core: &Shared, id: String) {
    {
        let mut guard = lock(core);
        guard.settings.installing = Some(id.clone());
        guard
            .settings
            .say(Notice::Info, "正在下载并校验;界面可以继续使用");
    }
    refresh(ui, core);
    let handle = ui.as_weak();
    let shared = Arc::clone(core);
    let spawned = std::thread::Builder::new()
        .name("settings-official".into())
        .spawn(move || {
            let result = settings_official::install(&id);
            let _ = handle.upgrade_in_event_loop(move |ui| {
                let mut guard = lock(&shared);
                guard.settings.installing = None;
                guard.settings.scripts = settings_actions::installed_scripts();
                guard.settings.ppocr_ready = settings_actions::ppocr_ready();
                match result {
                    Ok(message) => guard.settings.say(Notice::Success, message),
                    Err(error) => guard.settings.say(Notice::Error, error),
                }
                refresh(&ui, &shared);
            });
        });
    if let Err(error) = spawned {
        with(core, |settings| {
            settings.say(Notice::Error, format!("启动安装线程失败: {error}"));
        });
    }
}

/// 自检:写报告并返回成败;首行必须是「结果:通过/失败」。
pub fn selftest(report: &Path) -> Result<(), String> {
    let mut lines: Vec<String> = Vec::new();
    let result = selftest_run(&mut lines);
    lines.push(match &result {
        Ok(()) => "结果:通过".to_string(),
        Err(e) => format!("结果:失败 — {e}"),
    });
    let _ = std::fs::write(report, lines.join("\n"));
    result
}

/// 自检的正文:模型铺得开、关键控件在位、Slint 组件吃得进页面数据。
fn selftest_run(lines: &mut Vec<String>) -> Result<(), String> {
    use slint::Model;
    let mut settings = Settings::default();
    settings.current = update::current_version();
    let appearance = theme::Appearance::of_system();
    let colors = page_colors(&theme::colors_with(appearance, 0), appearance);
    let mut total = 0usize;
    for page in PageId::ALL {
        let view = settings.page_view(page, &colors);
        let rows: usize = view.groups.iter().map(|group| group.rows.len()).sum();
        if rows == 0 {
            return Err(format!("页面 {:?} 一行都没有", page));
        }
        total += rows;
    }
    let input = settings.page_view(PageId::Input, &colors);
    let aux_ok = input.groups.iter().any(|group| {
        group
            .rows
            .iter()
            .any(|row| row.label == "辅助码" && matches!(row.control, Control::Toggle { .. }))
    });
    if !aux_ok {
        return Err("输入页找不到辅助码开关".to_string());
    }
    lines.push(format!(
        "模型 {} 页共 {total} 行,辅助码开关在位",
        PageId::ALL.len()
    ));

    let core: Shared = Arc::new(Mutex::new(Core {
        settings,
        release: None,
    }));
    let ui = SettingsWindow::new().map_err(|e| format!("创建 Slint 组件: {e}"))?;
    refresh(&ui, &core);
    if ui.get_active_nav() != 0 || ui.get_groups().row_count() == 0 {
        return Err("Slint 组件里页面数据没有灌进去".to_string());
    }
    lines.push(format!(
        "Slint 组件创建成功,当前页 {} 组",
        ui.get_groups().row_count()
    ));
    Ok(())
}
