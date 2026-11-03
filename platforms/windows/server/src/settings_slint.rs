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
    Action, ConsentState, Notice, PageColors, PageId, Settings, UpdateState, WebdavField,
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
        Action::OpenOcrModels => with(core, settings_actions::open_ocr_models),
        Action::OpenApps => with(core, settings_actions::open_apps),
        Action::BackupExport => with(core, settings_actions::backup_export),
        Action::BackupImport => with(core, settings_actions::backup_import),
        Action::EditWebdav(field) => open_webdav_editor(ui, core, field),
        Action::SyncWebdav => start_webdav_sync(ui, core),
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

#[path = "settings_slint_dialogs.rs"]
mod dialogs;

use dialogs::{
    TextKind, add_app, choose_script, install_update, notice_kind, open_secret_editor,
    open_text_editor, open_webdav_editor, page_colors, slint_row, start_official_install,
    start_update_check, start_webdav_sync,
};

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
