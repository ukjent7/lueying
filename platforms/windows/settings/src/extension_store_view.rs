use super::*;
use crate::ext_store::{self, Official};

pub(super) fn detail(
    s: &State,
    id: &str,
    kit: &Kit,
    colors: &Colors,
    prefs: &PageColors,
    layers: &mut Vec<El>,
) {
    let settings = &s.core.settings;
    let official = ext_store::find(id);
    let installed = settings.scripts.iter().find(|script| script.id == id);
    let name = official
        .map(|item| item.name)
        .or_else(|| installed.map(|script| script.name.as_str()))
        .unwrap_or(id);
    let line = |value: String| {
        text(value)
            .font(TextStyle::new(13.0, 400.0).lh(20.0))
            .color(color(colors.text))
    };
    let mut content = col().key("extension-content").w_full().gap(8.0);
    if let Some(item) = official {
        content = content
            .child(line(item.detail.into()))
            .child(line(format!("建议快捷键：{}", item.shortcut)));
    }
    let close = KitButton::new("关闭")
        .variant(ButtonVariant::Outline)
        .on_press(|s: &mut State| s.overlay = Overlay::None)
        .build(kit);
    let mut buttons = row().gap(8.0).child(close);
    if let Some(script) = installed {
        if official.is_some() {
            content = content.child(line(format!("当前安装：{}", script.name)));
        }
        let enabled = settings.cfg.extensions.iter().any(|entry| entry == id);
        let approved = script.consent == ConsentState::Approved;
        if !approved {
            content = content.child(line(crate::settings::consent::prompt(script)));
        } else {
            content = content.child(line(
                if enabled {
                    "已启用"
                } else {
                    "已安装，尚未启用"
                }
                .into(),
            ));
        }
        let target = id.to_string();
        let label = if enabled && approved {
            "停用"
        } else if approved {
            "启用"
        } else {
            "同意并启用"
        };
        buttons = buttons.child(
            KitButton::new(label)
                .on_press(move |s: &mut State| change_enabled(s, &target, !enabled || !approved))
                .build(kit),
        );
        buttons = buttons.child(
            KitButton::new("保存")
                .variant(ButtonVariant::Contrast)
                .on_press(|s: &mut State| actions::save(&mut s.core.settings))
                .build(kit),
        );
        if enabled
            && approved
            && let Some(item) = official
        {
            let binding = settings
                .cfg
                .bindings
                .iter()
                .find(|(_, command)| command == item.command);
            if let Some((combo, _)) = binding {
                content = content.child(line(format!("已配置快捷键：{combo}")));
            } else {
                content = content.child(
                    KitButton::new("使用建议快捷键")
                        .variant(ButtonVariant::Outline)
                        .on_press(move |s: &mut State| bind_shortcut(&mut s.core.settings, item))
                        .build(kit),
                );
            }
        }
        let rows = settings
            .extension_settings(id)
            .into_iter()
            .flat_map(|group| group.rows)
            .chain(settings.extension_ocr_rows(id, prefs));
        for (index, setting) in rows.enumerate() {
            content =
                content.child(setting_row(&setting, kit, colors).key(format!("prefs-{index}")));
        }
    } else if let Some(item) = official {
        let downloading = s.downloading_extension == Some(item.id);
        let permissions = item.permission_preview();
        let can_install = permissions.is_ok();
        let summary = match permissions {
            Ok(items) if items.is_empty() => "不申请额外权限".into(),
            Ok(items) => format!("启用时会请求：{}", items.join("、")),
            Err(_) => "无法核对扩展权限，暂不可安装".into(),
        };
        content = content.child(line(summary));
        content = content.child(line(if downloading {
            "正在下载安装包…".into()
        } else {
            format!("官方可选下载 · v{} · 安装后再确认权限", item.version)
        }));
        buttons = buttons.child(
            KitButton::new(if downloading {
                "下载中"
            } else {
                "下载并安装"
            })
            .variant(ButtonVariant::Contrast)
            .disabled(!can_install || s.downloading_extension.is_some())
            .on_press(move |s: &mut State| download(s, item))
            .build(kit),
        );
    }
    if !settings.message.is_empty() {
        content = content.child(line(settings.message.clone()));
    }
    let body = el()
        .w_full()
        .max_h(355.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(content);
    let card = dialog_card("extension", name, vec![body], buttons, colors);
    mount_overlay("extension", card, layers);
}

fn bind_shortcut(settings: &mut Settings, item: Official) {
    use lueying_ext::host::bindings::parse;

    let Ok(hotkey) = parse(item.shortcut) else {
        return;
    };
    if settings
        .cfg
        .bindings
        .iter()
        .any(|(combo, _)| parse(combo).ok().as_ref() == Some(&hotkey))
    {
        settings.say(
            Notice::Error,
            "建议快捷键已分配给其他功能，没有替换原有绑定",
        );
        return;
    }
    settings
        .cfg
        .bindings
        .push((item.shortcut.into(), item.command.into()));
    settings.say(Notice::Info, "保存后可使用快捷键");
}

fn change_enabled(s: &mut State, id: &str, enabled: bool) {
    let settings = &mut s.core.settings;
    if enabled
        && settings
            .scripts
            .iter()
            .any(|script| script.id == id && script.consent != ConsentState::Approved)
    {
        if let Err(error) = actions::approve_package(id) {
            settings.say(Notice::Error, format!("同意失败: {error}"));
            return;
        }
        settings.scripts = actions::installed_scripts();
        if !settings
            .scripts
            .iter()
            .any(|script| script.id == id && script.consent == ConsentState::Approved)
        {
            settings.say(Notice::Error, "扩展已变更，请重新查看权限");
            return;
        }
    }
    settings.set_extension_enabled(id, enabled);
    if settings.message.is_empty() {
        settings.say(Notice::Info, "保存后生效");
    }
}

fn download(s: &mut State, item: Official) {
    let Some(handle) = ui_handle() else {
        return;
    };
    if s.downloading_extension.is_some()
        || s.core
            .settings
            .scripts
            .iter()
            .any(|script| script.id == item.id)
    {
        return;
    }
    s.downloading_extension = Some(item.id);
    s.core.settings.clear_notice();
    if let Err(error) = std::thread::Builder::new()
        .name("extension-store".into())
        .spawn(move || {
            let result = item.download();
            let scripts = actions::installed_scripts();
            handle.update(move |s: &mut State, _| {
                s.downloading_extension = None;
                s.core.settings.scripts = scripts;
                match result {
                    Ok(()) => s
                        .core
                        .settings
                        .say(Notice::Success, "已安装；同意权限并保存后可用"),
                    Err(error) => s.core.settings.say(Notice::Error, error),
                }
            });
        })
    {
        s.downloading_extension = None;
        s.core
            .settings
            .say(Notice::Error, format!("无法开始下载: {error}"));
    }
}

pub(super) fn selftest() -> Result<(), String> {
    type Rt = rune::Runtime<State>;

    fn click(rt: &mut Rt, rect: [f32; 4], offset: f32) {
        let [x, y, w, h] = rect;
        let x = x + w * offset;
        let y = y + h / 2.0;
        rt.pointer_move(x, y);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        rt.settle_all();
        rt.frame(0.0);
    }

    fn button(rt: &Rt, label: &str) -> Result<[f32; 4], String> {
        rt.elements()
            .into_iter()
            .find(|el| el.role == "button" && el.label.as_deref() == Some(label) && !el.inert)
            .map(|el| el.visible)
            .filter(|rect| rect[2] > 0.0 && rect[3] > 0.0)
            .ok_or_else(|| format!("扩展卡片找不到按钮:{label}"))
    }

    let mut state = State::default();
    state.core.settings.set_page(PageId::Extensions);
    let mut rt = Rt::new(state, view, (760.0, 480.0), rune::Env::new(false, 1.0));
    rt.frame(0.0);
    let card = rt
        .elements()
        .into_iter()
        .find(|el| el.key.as_deref() == Some("group-ext-store-0"))
        .ok_or("找不到截图识别卡片")?;
    click(&mut rt, card.visible, 0.2);
    if !matches!(&rt.state.overlay, Overlay::Extension(id) if id == "shot-ocr") {
        return Err("点截图识别卡片没有打开详情".into());
    }
    button(&rt, "下载并安装")?;
    if !rt.elements().iter().any(|el| {
        el.text
            .as_deref()
            .is_some_and(|s| s.contains("启用时会请求"))
    }) {
        return Err("下载前没有显示扩展权限".into());
    }

    rt.update(|s, _| {
        let mut shot = crate::settings::view::ScriptEntry::new("shot-ocr", "截图识别");
        shot.consent = ConsentState::Missing;
        s.core.settings.scripts.push(shot);
    });
    rt.frame(1.0 / 60.0);
    button(&rt, "同意并启用")?;
    if !rt.state.core.settings.cfg.extensions.is_empty() {
        return Err("扩展未授权就已启用".into());
    }

    rt.update(|s, _| s.core.settings.scripts[0].consent = ConsentState::Approved);
    rt.frame(1.0 / 60.0);
    rt.settle_all();
    rt.frame(0.0);
    let enable = button(&rt, "启用")?;
    click(&mut rt, enable, 0.5);
    if rt.state.core.settings.cfg.extensions != ["shot-ocr"] {
        return Err("点启用没有选中截图识别".into());
    }
    let shortcut = button(&rt, "使用建议快捷键")?;
    click(&mut rt, shortcut, 0.5);
    let settings = &rt.state.core.settings;
    if settings.cfg.extensions != ["shot-ocr"]
        || !settings
            .cfg
            .bindings
            .iter()
            .any(|(_, command)| command == "shot-ocr.run")
        || !settings.saved.extensions.is_empty()
    {
        return Err("启用或快捷键没有写入待保存的设置".into());
    }
    Ok(())
}
