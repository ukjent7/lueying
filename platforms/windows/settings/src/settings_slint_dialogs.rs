//! 设置窗动作:文件/目录选择、密钥与文本编辑、更新检查与安装。
//!
//! 从 `settings_slint.rs` 挪出的对话框与后台任务;`Core`/`with`/`items`
//! 等窗口骨架仍在父模块。

use super::*;
use lueying_server::ext_secret;

/// 提示颜色反查种类:0 info 1 success 2 error。
pub(super) fn notice_kind(tint: u32, colors: &PageColors) -> i32 {
    if tint == colors.notice_success {
        1
    } else if tint == colors.notice_error {
        2
    } else {
        0
    }
}

pub(super) fn slint_button(button: &model::Button) -> Btn {
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

pub(super) fn slint_row(row: &model::Row) -> Row {
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
pub(super) fn color(rgb: u32) -> slint::Color {
    slint::Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// 页面里要用的颜色:提示色与主题色选项的着色。
pub(super) fn page_colors(colors: &theme::Colors, appearance: theme::Appearance) -> PageColors {
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
pub(super) fn choose_script(ui: &SettingsWindow, core: &Shared) {
    let scripts = {
        let mut guard = lock(core);
        guard.settings.scripts = actions::installed_scripts();
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
                let text = crate::settings::consent::prompt(script);
                if !confirm(&text, "扩展授权") {
                    lock(core)
                        .settings
                        .say(Notice::Info, format!("未同意 {name},扩展保持停用"));
                    return;
                }
                if let Err(error) = actions::approve_package(&id) {
                    lock(core)
                        .settings
                        .say(Notice::Error, format!("同意失败: {error}"));
                    return;
                }
                approved_now = true;
            }
            let mut guard = lock(core);
            if approved_now {
                guard.settings.scripts = actions::installed_scripts();
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
pub(super) fn add_app(ui: &SettingsWindow, core: &Shared) {
    let apps = {
        let guard = lock(core);
        actions::running_apps(&guard.settings.cfg.apps)
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
pub(super) fn open_secret_editor(
    ui: &SettingsWindow,
    core: &Shared,
    ext: String,
    key: String,
    title: String,
) {
    let Ok(editor) = SecretEditorWindow::new() else {
        return;
    };
    editor.set_win_title(format!("扩展密钥 · {title}").into());
    editor.set_hint("只保存在本机;留空并保存即删除".into());
    editor.set_body(ext_secret::get(&ext, &key).unwrap_or_default().into());
    let main = ui.as_weak();
    let shared = Arc::clone(core);
    let weak = editor.as_weak();
    editor.on_save(move || {
        let Some(editor) = weak.upgrade() else {
            return;
        };
        let text = editor.get_body().to_string();
        let _ = editor.hide();
        {
            let mut guard = lock(&shared);
            match ext_secret::set(&ext, &key, &text) {
                Ok(()) => guard.settings.say(Notice::Success, "已保存扩展密钥"),
                Err(error) => guard
                    .settings
                    .say(Notice::Error, format!("保存失败: {error}")),
            }
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
pub(super) enum TextKind {
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
pub(super) fn open_text_editor(ui: &SettingsWindow, core: &Shared, kind: TextKind) {
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
                actions::reload();
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
pub(super) fn start_update_check(ui: &SettingsWindow, core: &Shared) {
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
pub(super) fn install_update(ui: &SettingsWindow, core: &Shared) {
    let release = {
        let mut guard = lock(core);
        let Some(release) = guard.release.clone() else {
            return;
        };
        if !actions::install_update(&mut guard.settings, &release) {
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
            let result = actions::download_and_run_installer(&release);
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
