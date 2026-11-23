//! 设置窗:Rune 承载页面,视觉与候选窗同一框架。页面模型在
//! `lueying_settings::settings::view`,这里负责窗口、控件与后台任务接线。
//!
//! 弹层(list / 多行文本 / 密钥 / 确认)都是主窗里的 scrim + 居中卡片,
//! 不开第二个 OS 窗口。`--selftest <报告路径>` 校验页面模型与控件树。

use std::cell::RefCell;
use std::path::Path;

use lueying_protocol::Request;
use rune::tok::motion;
use rune::{
    App, AppHandle, AppSpec, Color, Cursor, Cx, Drag, El, Phase, ScrollSpec, TextEdit, TextStyle,
    col, el, row, text,
};
use rune_kit::{
    Button as KitButton, ButtonVariant, ControlSize, Kit, PaletteSeed, SegmentedControl, Switch,
    TextArea, TextInput, root,
};

use crate::ext_secret;
use crate::settings::actions;
use crate::settings::model::{self as model, ButtonKind, Control};
use crate::settings::view::{Action, ConsentState, Notice, PageColors, PageId};
use crate::settings::view::{Settings, UpdateState};
use crate::update::{self, Check, Release};
use lueying_server::accents::ACCENTS;
use lueying_server::config::{self, AppConfig};
use lueying_server::theme::{self, Colors};

#[path = "extension_store_view.rs"]
mod store;

/// 窗口状态:配置与平台侧数据;每次控件回调直接改它,下一帧自动反映。
#[derive(Default)]
struct Core {
    settings: Settings,
    /// 待安装的新版;检查更新成功后填入。
    release: Option<Release>,
}

#[derive(Default)]
enum Overlay {
    #[default]
    None,
    Extension(String),
    Picker {
        title: String,
        items: Vec<String>,
        action: PickerAction,
    },
    Secret {
        title: String,
        ext: String,
        key: String,
    },
    Text {
        title: String,
        path: std::path::PathBuf,
        kind: TextKind,
    },
    Confirm {
        title: String,
        message: String,
        action: ConfirmAction,
    },
}

#[derive(Clone, Copy)]
enum PickerAction {
    Script,
    AddApp,
}

#[derive(Clone)]
enum ConfirmAction {
    Install,
    /// 同意某个扩展后再选中它。
    Consent {
        id: String,
        name: String,
        index: usize,
    },
}

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
            Self::Phrases => lueying_core::features::phrases::TEMPLATE,
            Self::Punct => config::PUNCT_TEMPLATE,
        }
    }
}

#[derive(Default)]
struct State {
    core: Core,
    /// 搜索框编辑器;文字变化直接写回 `settings.search`。
    search: TextEdit,
    /// 密钥与文本编辑弹层共用的编辑器。
    secret: TextEdit,
    text: TextEdit,
    overlay: Overlay,
    downloading_extension: Option<&'static str>,
}

thread_local! {
    /// UI 线程保存句柄;控件回调用它启动后台任务或关窗。
    static HANDLE: RefCell<Option<AppHandle<State>>> = const { RefCell::new(None) };
}

fn ui_handle() -> Option<AppHandle<State>> {
    HANDLE.with(|handle| handle.borrow().clone())
}

/// 打开设置窗并跑事件循环;窗口关掉才返回。
pub fn run() {
    let state = State {
        core: load_core(),
        ..State::default()
    };
    let spec = AppSpec {
        families: ("Noto Sans SC", "Noto Sans SC"),
        min_size: Some((760.0, 480.0)),
        // 弹层全靠 rune 原生机制:卡片是 `.modal(...)`(Esc 关、点外关、焦点圈住),
        // 选择器是 `listbox` 焦点组(方向键巡游、回车/空格确认),无需 on_key。
        on_key: None,
        ..AppSpec::new("掠影设置", (960.0, 680.0), State::default, view)
    };
    let app = App::new(spec).init(move || state);
    let handle = app.handle();
    HANDLE.with(|slot| *slot.borrow_mut() = Some(handle.clone()));
    // 重排模型不随安装包分发;打开设置窗时后台补齐,已就绪则不发请求。
    start_model_fetch(handle);
    app.run();
}

/// 启动时读配置与平台侧数据;配置问题写进页脚提示。
fn load_core() -> Core {
    let path = config::path();
    let cfg = path
        .as_ref()
        .map_or_else(AppConfig::default, |p| config::load(p));
    let mut settings = Settings::with_config(cfg);
    if let Some(issue) = settings.cfg.config_issues.first() {
        settings.say(
            Notice::Error,
            format!(
                "配置有 {} 处问题:{} ({})",
                settings.cfg.config_issues.len(),
                issue.message,
                issue.layer
            ),
        );
    }
    settings.path = path;
    settings.groups = actions::call(&Request::FuzzyStats)
        .map(|reply| reply.groups)
        .unwrap_or_default();
    settings.status = actions::call(&Request::Status).and_then(|reply| reply.status);
    settings.current = update::current_version();
    settings.scripts = actions::installed_scripts();
    settings.ppocr_ready = actions::ppocr_ready();
    settings.ext_metrics = actions::read_ext_metrics();
    Core {
        settings,
        release: None,
    }
}

fn view(s: &State, cx: &mut Cx<State>) -> El {
    // 系统关掉"动画效果"时,框架把弹簧动效降级为淡出。
    cx.reduced_motion(!theme::client_area_animation());
    scene(s)
}

/// 0xRRGGBB 转线性 sRGB 颜色;转换共享一份,见 `lueying_server::theme::srgb`。
fn color(rgb: u32) -> Color {
    theme::srgb(rgb)
}

/// 页面需要的提示色与主题色选项,顺序同 `ACCENTS`。
fn page_colors(c: &Colors, appearance: theme::Appearance) -> PageColors {
    PageColors {
        notice_info: c.text_secondary,
        notice_success: c.success,
        notice_error: c.danger,
        accents: ACCENTS
            .iter()
            .map(|accent| match appearance {
                theme::Appearance::Dark => accent.dark[0],
                _ => accent.light[0],
            })
            .collect(),
    }
}

fn scene(s: &State) -> El {
    let appearance = theme::Appearance::of_system();
    // 固定暖黑瓷:瓷是身份材质,不跟系统深浅走;高对比度让位系统色。
    let colors = if matches!(appearance, theme::Appearance::HighContrast) {
        theme::colors_with(appearance, 0)
    } else {
        theme::ceramic_colors()
    };
    let kit = Kit::from_palette(PaletteSeed {
        bg: color(colors.bg),
        fg: color(colors.text),
        accent: color(colors.accent),
    });
    let page_colors = page_colors(&colors, appearance);
    let page = s.core.settings.page(&page_colors);
    let notice_color = if page.notice_tint == page_colors.notice_error {
        colors.danger
    } else if page.notice_tint == page_colors.notice_success {
        colors.success
    } else {
        colors.text_secondary
    };

    let header = row()
        .key("header")
        .w_full()
        .h(52.0)
        .px(18.0)
        .gap(16.0)
        .center_y()
        .child(
            col()
                .key("titles")
                .gap(1.0)
                .child(
                    text("掠影设置")
                        .font(TextStyle::new(16.0, 600.0))
                        .color(color(colors.text)),
                )
                .child(
                    text(page.subtitle.clone())
                        .font(TextStyle::new(11.0, 400.0))
                        .color(color(colors.text_secondary)),
                ),
        )
        .child(
            TextInput::new(&s.search)
                .placeholder("搜索设置")
                .label("搜索设置")
                .width(Some(240.0))
                .on_edit(|s: &mut State| &mut s.search)
                .on_change(|s: &mut State, _| {
                    let query = s.search.text.clone();
                    s.core.settings.set_search(query);
                })
                .build(&kit)
                .key("search"),
        )
        .child(
            page.header_button
                .as_ref()
                .map_or_else(el, |button| button_el(button, &kit)),
        );

    // 侧栏导航是一个 list 焦点组:Tab 只停一次,方向键在条目间巡游,
    // 回车/空格激活,选中项即 a11y 的 selected。
    let n_nav = page.nav.len();
    let mut nav = col()
        .key("nav")
        .role("list")
        .label("设置分类")
        .focus_group()
        .w(148.0)
        .gap(3.0)
        .px(10.0)
        .py(14.0);
    for (i, item) in page.nav.iter().enumerate() {
        let selected = i == page.active_nav;
        let base = item.id.base.clone();
        nav = nav.child(
            row()
                .key(format!("nav-{}", base))
                .role("listitem")
                .label(item.text.clone())
                .position_in_set(i + 1, n_nav)
                .focusable(true)
                .focus_selected(selected)
                .w_full()
                .h(34.0)
                .px(12.0)
                .radius(8.0)
                .center_y()
                .fill(if selected {
                    color(colors.selected_bg)
                } else {
                    Color::TRANSPARENT
                })
                .hover(color(colors.hover))
                .on_press_tx(move |s: &mut State, _| activate(s, base.clone(), 0))
                .child(
                    text(item.text.clone())
                        .font(TextStyle::new(13.0, if selected { 500.0 } else { 400.0 }))
                        .color(color(if selected {
                            colors.accent_text
                        } else {
                            colors.text
                        })),
                ),
        );
    }

    let body = row()
        .key("body")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(nav)
        .child(page_scene(&page, &kit, &colors));

    let mut footer = row()
        .key("footer")
        .w_full()
        .min_h(48.0)
        .px(18.0)
        .gap(10.0)
        .center_y()
        .child(
            text(page.notice.clone())
                .key("message")
                .font(TextStyle::new(12.0, 400.0))
                .color(color(notice_color))
                .grow(1.0)
                .truncate(),
        );
    for button in &page.footer_buttons {
        footer = footer.child(button_el(button, &kit));
    }

    let base = col()
        .key("window")
        .role("window")
        .w_full()
        .h_full()
        .fill(color(colors.bg))
        .child(header)
        .child(body)
        .child(footer);

    let mut layers = Vec::new();
    overlay_scene(s, &s.overlay, &kit, &colors, &page_colors, &mut layers);
    root(base, layers)
}

/// 右栏:滚动的卡片流。
fn page_scene(page: &model::Page, kit: &Kit, colors: &Colors) -> El {
    let mut cards = col().key("cards").w_full().gap(12.0);
    for group in &page.groups {
        cards = cards.child(group_card(group, kit, colors));
    }
    col()
        .key("page")
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .px(14.0)
        .pb(14.0)
        // 滚动边缘渐隐,官方 settings 示例的内容列同款:提示还有没滚到的卡片。
        .clip_fade(0.0, 8.0, 0.0, 16.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(cards)
}

fn group_card(group: &model::Group, kit: &Kit, colors: &Colors) -> El {
    let extension = match group.rows.as_slice() {
        [
            model::Row {
                control: Control::Buttons { items },
                ..
            },
        ] => items.first().filter(|button| {
            matches!(
                button.id.base.as_str(),
                crate::settings::view::ids::EXT_STORE | crate::settings::view::ids::EXT_INSTALLED
            )
        }),
        _ => None,
    };
    let key = extension.map_or_else(
        || group.title.clone(),
        |button| format!("{}-{}", button.id.base, button.id.index),
    );
    // 卡片铺在窗底上:上下缘的釉光从窗底色算出,环境亮则缘亮,没有凭空的光。
    let glaze = theme::glaze_from_ambient(color(colors.bg), color(colors.bg));
    let mut card = col()
        .key(format!("group-{key}"))
        .w_full()
        .radius(12.0)
        .fill(color(colors.surface))
        .border(1.0, color(colors.border));
    if let Some(button) = extension {
        let id = button.id.clone();
        card = card
            .cursor(Cursor::Pointer)
            .on_press(move |s: &mut State| activate(s, id.base.clone(), id.index));
    }
    card = card
        .child(
            el().pin(Some(0.0), Some(0.0), Some(0.0), None)
                .h(1.0)
                .fill(glaze.edge_top)
                .a11y_hidden(),
        )
        .child(
            el().pin(Some(0.0), None, Some(0.0), Some(0.0))
                .h(1.0)
                .fill(glaze.edge_bottom)
                .a11y_hidden(),
        );
    let mut inner = col().w_full().padding(10.0);
    for (i, row_data) in group.rows.iter().enumerate() {
        inner = inner.child(setting_row(row_data, kit, colors).key(format!("row-{i}")));
        if i + 1 < group.rows.len() {
            inner = inner.child(
                el().h(1.0)
                    .ml(10.0)
                    .fill(color(colors.border))
                    .a11y_hidden(),
            );
        }
    }
    card = card.child(inner);
    col()
        .key(format!("section-{key}"))
        .w_full()
        .gap(6.0)
        .child(
            text(group.title.clone())
                .font(TextStyle::new(11.0, 500.0))
                .color(color(colors.text_secondary))
                .ml(4.0),
        )
        .child(card)
}

fn setting_row(row_data: &model::Row, kit: &Kit, colors: &Colors) -> El {
    let info = || {
        col()
            .key("info")
            .grow(1.0)
            .min_w(0.0)
            .gap(2.0)
            .child(
                text(row_data.label.clone())
                    .font(TextStyle::new(13.0, 400.0).lh(18.0))
                    .color(color(colors.text))
                    .truncate(),
            )
            .child(
                text(row_data.detail.clone())
                    .font(TextStyle::new(11.0, 400.0).lh(15.0))
                    .color(color(colors.text_secondary))
                    .truncate(),
            )
    };
    match &row_data.control {
        Control::Preview { style, .. } => col()
            .key("preview-row")
            .w_full()
            .gap(8.0)
            .px(10.0)
            .py(8.0)
            .child(info())
            .child(candidate_preview(style, colors)),
        _ => row()
            .key("setting-row")
            .w_full()
            .min_h(52.0)
            .px(10.0)
            .py(5.0)
            .gap(12.0)
            .center_y()
            .child(info())
            .child(control_el(&row_data.control, kit, colors, &row_data.label)),
    }
}

/// 候选窗预览:直接画拼音与胶囊候选,选中项用强调色。
fn candidate_preview(style: &model::CandidatePreview, colors: &Colors) -> El {
    let font = TextStyle::new(12.0 * style.font_scale, 400.0);
    let mut candidates = if style.vertical { col() } else { row() };
    candidates = candidates.key("candidates").gap(4.0);
    for (i, candidate) in style
        .candidates
        .iter()
        .take(style.page_size.max(1))
        .enumerate()
    {
        let selected = i == 0;
        candidates = candidates.child(
            row()
                .key(format!("candidate-{i}"))
                .px(9.0)
                .py(3.0)
                .radius(6.0)
                .fill(if selected {
                    color(style.accent)
                } else {
                    color(colors.surface_alt)
                })
                .child(text(candidate.clone()).font(font).color(color(if selected {
                    colors.on_accent
                } else {
                    colors.text
                }))),
        );
    }
    col()
        .key("candidate-preview")
        .w_full()
        .gap(6.0)
        .px(10.0)
        .py(8.0)
        .radius(10.0)
        .fill(color(colors.surface_alt))
        .border(1.0, color(colors.border))
        .child(
            text(style.pinyin.clone())
                .font(TextStyle::new(11.0, 400.0))
                .color(color(colors.text_secondary)),
        )
        .child(candidates)
}

fn control_el(control: &Control, kit: &Kit, colors: &Colors, label: &str) -> El {
    match control {
        Control::Toggle { id, on } => {
            let base = id.base.clone();
            Switch::new(*on)
                .on_toggle(move |s: &mut State| activate(s, base.clone(), 0))
                .build(kit)
        }
        Control::Segment {
            id,
            options,
            selected,
            cell_width,
        } => {
            let labels: Vec<String> = options.iter().map(|option| option.text.clone()).collect();
            let base = id.base.clone();
            let group = SegmentedControl::new(&labels, Some(*selected));
            let group = match cell_width {
                Some(width) => group.width(Some(*width * labels.len() as f32)),
                None => group.fit(),
            };
            group
                .on_pick(move |s: &mut State, index, tx| {
                    // 主题色这类分段切换会让整页换色;官方主题变化用 CALM 弹簧渐变。
                    tx.color_motion(motion::CALM);
                    activate(s, base.clone(), index)
                })
                .build(kit)
        }
        Control::Slider { id, value } => slider_el(id, label, *value, colors),
        Control::Buttons { items } => buttons_el(items, kit),
        Control::ButtonsToggle {
            items,
            toggle,
            toggle_on,
        } => {
            let base = toggle.base.clone();
            row()
                .key("buttons-toggle")
                .gap(8.0)
                .center_y()
                .child(buttons_el(items, kit))
                .child(
                    Switch::new(*toggle_on)
                        .on_toggle(move |s: &mut State| activate(s, base.clone(), 0))
                        .build(kit),
                )
        }
        Control::Preview { style, .. } => candidate_preview(style, colors),
        Control::None => el(),
    }
}

/// 滑杆:轨道 + 左侧强调色高亮段 + 圆形拇指;拖动即改值。
/// 取值 0..=1,保存按钮把整份配置落盘。写法对齐官方 settings 示例的滑杆:
/// a11y 走 role/range_value/on_step(方向键 0.05 步进),按下记值、Esc 取消恢复,
/// 点在轨道远处用弹簧滑过去,拖动中 tx.instant() 让拇指严格跟手。
fn slider_el(id: &model::Id, label: &str, value: f32, colors: &Colors) -> El {
    const TRACK_W: f32 = 180.0;
    const TRACK_H: f32 = 4.0;
    const THUMB: f32 = 20.0;
    const TRAVEL: f32 = TRACK_W - THUMB;
    const STEP: f64 = 0.05;
    let base = id.base.clone();
    let value = value.clamp(0.0, 1.0);
    let to_value = |x: f32| ((x - THUMB * 0.5) / TRAVEL).clamp(0.0, 1.0);
    row()
        .key(format!("slider-{}", id.base))
        .w(TRACK_W)
        .h(THUMB)
        .center_y()
        .cursor(Cursor::Pointer)
        .role("slider")
        .label(label.to_string())
        .range_value(f64::from(value), 0.0, 1.0)
        .range_step(STEP)
        .a11y_value(format!("{:.0}%", value * 100.0))
        .on_step({
            let base = base.clone();
            move |s: &mut State, steps, _| {
                let current = s.core.settings.slider_value(&base);
                s.core.settings.set_slider(
                    &base,
                    (current + steps as f32 * STEP as f32).clamp(0.0, 1.0),
                );
            }
        })
        .motion(motion::SNAPPY)
        .on_drag(move |s: &mut State, drag: &mut Drag, tx| {
            let current = s.core.settings.slider_value(&base);
            match drag.phase {
                Phase::Down => {
                    drag.data = current;
                    let knob = THUMB * 0.5 + current * TRAVEL;
                    if (drag.x - knob).abs() > THUMB * 0.5 + 2.0 {
                        // 点在轨道上:拇指用弹簧滑过去。
                        s.core.settings.set_slider(&base, to_value(drag.x));
                    }
                }
                Phase::Move => {
                    let knob = THUMB * 0.5 + drag.data * TRAVEL;
                    let grab = if (drag.start_x - knob).abs() <= THUMB * 0.5 + 2.0 {
                        drag.start_x - knob
                    } else {
                        0.0
                    };
                    s.core.settings.set_slider(&base, to_value(drag.x - grab));
                    tx.instant();
                }
                Phase::Up => {
                    if drag.cancelled() {
                        s.core.settings.set_slider(&base, drag.data);
                    }
                }
            }
        })
        .child(
            el().absolute(0.0, (THUMB - TRACK_H) * 0.5)
                .w(TRACK_W)
                .h(TRACK_H)
                .radius(TRACK_H * 0.5)
                .fill(color(colors.border))
                .a11y_hidden()
                .child(
                    el().absolute(0.0, 0.0)
                        .w(TRAVEL * value + THUMB * 0.5)
                        .h(TRACK_H)
                        .radius(TRACK_H * 0.5)
                        .fill(color(colors.accent)),
                ),
        )
        .child(
            el().absolute(TRAVEL * value, 0.0)
                .w(THUMB)
                .h(THUMB)
                .radius(THUMB * 0.5)
                .fill(color(colors.bg))
                .border(1.0, color(colors.border))
                .shadow(0.0, 5.0, Color::BLACK.alpha(0.22))
                .a11y_hidden(),
        )
}

fn buttons_el(items: &[model::Button], kit: &Kit) -> El {
    let mut buttons = row().key("buttons").gap(6.0).center_y();
    for button in items {
        buttons = buttons.child(button_el(button, kit));
    }
    buttons
}

fn button_el(button: &model::Button, kit: &Kit) -> El {
    let variant = match button.kind {
        ButtonKind::Primary => ButtonVariant::Contrast,
        ButtonKind::Secondary | ButtonKind::Ghost => ButtonVariant::Outline,
    };
    let base = button.id.base.clone();
    let index = button.id.index;
    KitButton::new(button.text.clone())
        .variant(variant)
        .control_size(ControlSize::Small)
        .disabled(!button.enabled)
        .on_press(move |s: &mut State| activate(s, base.clone(), index))
        .build(kit)
}

/// 一次激活:先让模型改配置,再执行它要求的平台动作。
fn activate(s: &mut State, base: String, index: usize) {
    let id = model::Id::new(base.clone(), index);
    let switched = PageId::from_nav(&base).is_some();
    let action = s.core.settings.activate(&id);
    if switched {
        s.core.settings.clear_notice();
    }
    if let Some(action) = action {
        dispatch(s, action);
    }
}

fn dispatch(s: &mut State, action: Action) {
    match action {
        Action::Close => {
            if let Some(handle) = ui_handle() {
                handle.close();
            }
        }
        Action::Save => actions::save(&mut s.core.settings),
        Action::OpenScripts => actions::open_scripts(&mut s.core.settings),
        Action::OpenTypingLog => actions::open_typing_log(&mut s.core.settings),
        Action::ClearTypingLog => actions::clear_typing_log(&mut s.core.settings),
        Action::ImportLuext => actions::import_luext(&mut s.core.settings),
        Action::ApproveExtension => actions::approve_extension(&mut s.core.settings),
        Action::RefreshExtMetrics => actions::refresh_ext_metrics(&mut s.core.settings),
        Action::OpenExtension(id) => {
            s.core.settings.clear_notice();
            s.overlay = Overlay::Extension(id);
        }
        Action::OpenOcrModels => actions::open_ocr_models(&mut s.core.settings),
        Action::OpenApps => actions::open_apps(&mut s.core.settings),
        Action::BackupExport => actions::backup_export(&mut s.core.settings),
        Action::BackupImport => actions::backup_import(&mut s.core.settings),
        Action::ImportUser => actions::import_user(&mut s.core.settings),
        Action::ExportUser => actions::export_user(&mut s.core.settings),
        Action::Diagnostics => actions::export_diagnostics(&mut s.core.settings),
        Action::ChooseScript => choose_script(s),
        Action::AddApp => add_app(s),
        Action::EditSecret(ext, key, title) => open_secret(s, ext, key, title),
        Action::OpenPhrases => open_text(s, TextKind::Phrases),
        Action::OpenPunct => open_text(s, TextKind::Punct),
        Action::CheckUpdate => start_update_check(s),
        Action::InstallUpdate(_) => confirm_install(s),
    }
}

/// 选运行期扩展:关闭、直接选中,或先弹同意确认。
fn choose_script(s: &mut State) {
    s.core.settings.scripts = actions::installed_scripts();
    if s.core.settings.scripts.is_empty() {
        s.core
            .settings
            .say(Notice::Info, "没有已安装的扩展;点「目录」放入扩展包");
        return;
    }
    let mut items = vec!["关闭运行期扩展".to_string()];
    items.extend(
        s.core
            .settings
            .scripts
            .iter()
            .map(|script| format!("{}  {}", script.name, script.id)),
    );
    s.overlay = Overlay::Picker {
        title: "选择运行期扩展".into(),
        items,
        action: PickerAction::Script,
    };
}

fn add_app(s: &mut State) {
    let apps = actions::running_apps(&s.core.settings.cfg.apps);
    if apps.is_empty() {
        s.core.settings.say(
            Notice::Info,
            "没有可添加的运行中程序;可打开「配置文件」手动添加",
        );
        return;
    }
    s.overlay = Overlay::Picker {
        title: "添加应用".into(),
        items: apps
            .iter()
            .map(|(exe, title)| format!("{title}  {exe}"))
            .collect(),
        action: PickerAction::AddApp,
    };
}

fn pick(s: &mut State, action: PickerAction, index: usize) {
    s.overlay = Overlay::None;
    match action {
        PickerAction::AddApp => {
            let apps = actions::running_apps(&s.core.settings.cfg.apps);
            let Some((exe, title)) = apps.get(index) else {
                return;
            };
            s.core
                .settings
                .cfg
                .apps
                .insert(exe.clone(), lueying_server::config::AppOverride::default());
            s.core
                .settings
                .say(Notice::Info, format!("已添加 {title} ({exe});保存后生效"));
        }
        PickerAction::Script => {
            if index == 0 {
                s.core.settings.select_script(None);
                s.core
                    .settings
                    .say(Notice::Info, "已关闭运行期扩展,保存后生效");
                return;
            }
            let Some(script) = s.core.settings.scripts.get(index - 1).cloned() else {
                return;
            };
            let name = script.name.clone();
            if script.consent != ConsentState::Approved {
                s.overlay = Overlay::Confirm {
                    title: "扩展授权".into(),
                    message: crate::settings::consent::prompt(&script),
                    action: ConfirmAction::Consent {
                        id: script.id,
                        name,
                        index: index - 1,
                    },
                };
                return;
            }
            s.core.settings.select_script(Some(index - 1));
            s.core
                .settings
                .say(Notice::Info, format!("已选择 {name},保存后生效"));
        }
    }
}

fn confirm(s: &mut State, action: ConfirmAction) {
    s.overlay = Overlay::None;
    match action {
        ConfirmAction::Install => {
            let Some(release) = s.core.release.clone() else {
                return;
            };
            s.core
                .settings
                .say(Notice::Info, "正在下载安装包;界面可以继续使用");
            if let Some(handle) = ui_handle() {
                let spawned = std::thread::Builder::new()
                    .name("settings-update".into())
                    .spawn(move || {
                        let result = actions::download_and_run_installer(&release);
                        handle.update(move |s: &mut State, _| match result {
                            Ok(()) => s
                                .core
                                .settings
                                .say(Notice::Success, "安装程序已启动;按提示完成更新"),
                            Err(error) => s
                                .core
                                .settings
                                .say(Notice::Error, format!("更新失败: {error}")),
                        });
                    });
                if let Err(error) = spawned {
                    s.core
                        .settings
                        .say(Notice::Error, format!("启动下载线程失败: {error}"));
                }
            }
        }
        ConfirmAction::Consent { id, name, index } => {
            if let Err(error) = actions::approve_package(&id) {
                s.core
                    .settings
                    .say(Notice::Error, format!("同意失败: {error}"));
                return;
            }
            s.core.settings.scripts = actions::installed_scripts();
            s.core.settings.select_script(Some(index));
            s.core
                .settings
                .say(Notice::Info, format!("已同意 {name},保存后生效"));
        }
    }
}

fn open_secret(s: &mut State, ext: String, key: String, title: String) {
    s.secret = TextEdit::new(ext_secret::get(&ext, &key).unwrap_or_default());
    s.overlay = Overlay::Secret { title, ext, key };
}

fn save_secret(s: &mut State, ext: String, key: String) {
    let value = s.secret.text.clone();
    s.overlay = Overlay::None;
    match ext_secret::set(&ext, &key, &value) {
        Ok(()) => s.core.settings.say(Notice::Success, "已保存扩展密钥"),
        Err(error) => s
            .core
            .settings
            .say(Notice::Error, format!("保存失败: {error}")),
    }
}

fn open_text(s: &mut State, kind: TextKind) {
    let Some(path) = kind.path() else {
        s.core.settings.say(Notice::Error, "没有可写的文本文件路径");
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if !path.exists()
        && let Err(error) = std::fs::write(&path, kind.template())
    {
        s.core
            .settings
            .say(Notice::Error, format!("写模板失败: {error}"));
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(body) => {
            s.text = TextEdit::new(body);
            s.overlay = Overlay::Text {
                title: kind.title().into(),
                path,
                kind,
            };
        }
        Err(error) => s
            .core
            .settings
            .say(Notice::Error, format!("读文件失败: {error}")),
    }
}

fn save_text(s: &mut State, path: std::path::PathBuf, kind: TextKind) {
    let body = s.text.text.clone();
    s.overlay = Overlay::None;
    match write_text(&path, &body) {
        Ok(()) => {
            actions::reload();
            s.core
                .settings
                .say(Notice::Success, format!("已保存{}", kind.title()));
        }
        Err(error) => s.core.settings.say(
            Notice::Error,
            format!("写入 {} 失败: {error}", path.display()),
        ),
    }
}

/// 临时文件 + fsync + 原子改名:断电不会留下半截文件。
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

/// 后台查更新:网络慢不能卡住窗口;结果回 UI 线程再改状态。
fn start_update_check(s: &mut State) {
    if matches!(s.core.settings.updates, UpdateState::Checking) {
        return;
    }
    let channel = match s.core.settings.cfg.update_channel {
        config::Channel::Beta => update::Channel::Beta,
        config::Channel::Stable => update::Channel::Stable,
    };
    s.core.settings.updates = UpdateState::Checking;
    let Some(handle) = ui_handle() else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("settings-update".into())
        .spawn(move || {
            let result = update::check_channel(channel);
            handle.update(move |s: &mut State, _| {
                s.core.settings.updates = match result {
                    Ok(Check::Latest) => UpdateState::Latest,
                    Ok(Check::Available(release)) => {
                        let version = release.version.clone();
                        s.core.release = Some(release);
                        UpdateState::Available(version)
                    }
                    Ok(Check::NoRelease) => UpdateState::NoRelease,
                    Err(error) => UpdateState::Failed(error),
                };
            });
        });
    if let Err(error) = spawned {
        s.core
            .settings
            .say(Notice::Error, format!("检查更新的线程启动失败: {error}"));
    }
}

fn confirm_install(s: &mut State) {
    let Some(release) = s.core.release.clone() else {
        return;
    };
    s.overlay = Overlay::Confirm {
        title: "检查更新".into(),
        message: format!(
            "安装掠影 {}？\n\n安装包会下载到临时目录,校验后直接运行安装程序。",
            release.version
        ),
        action: ConfirmAction::Install,
    };
}

/// 启动时后台补齐重排模型;真下到了才让常驻服务带新模型重启。
fn start_model_fetch(handle: AppHandle<State>) {
    let spawned = std::thread::Builder::new()
        .name("settings-models".into())
        .spawn(move || {
            let result = crate::models::ensure();
            if result.as_ref().is_ok_and(|&count| count > 0) {
                actions::restart_server();
            }
            handle.update(move |s: &mut State, _| {
                s.core.settings.status =
                    actions::call(&Request::Status).and_then(|reply| reply.status);
                match result {
                    Ok(0) => {}
                    Ok(count) => s.core.settings.say(
                        Notice::Success,
                        format!("重排模型已就绪(补齐 {count} 个文件)"),
                    ),
                    Err(error) => s
                        .core
                        .settings
                        .say(Notice::Error, format!("重排模型补齐失败: {error}")),
                }
            });
        });
    if let Err(error) = spawned {
        eprintln!("补齐重排模型的线程启动失败: {error}");
    }
}
/// 主窗内的弹层:全窗 scrim + 居中卡片,四种弹层共用。
fn overlay_scene(
    s: &State,
    overlay: &Overlay,
    kit: &Kit,
    colors: &Colors,
    prefs: &PageColors,
    layers: &mut Vec<El>,
) {
    let (key, title, body): (&str, String, Vec<El>) = match overlay {
        Overlay::None => return,
        Overlay::Extension(id) => {
            store::detail(s, id, kit, colors, prefs, layers);
            return;
        }
        Overlay::Picker {
            title,
            items,
            action,
        } => {
            // 原生 listbox:方向键巡游焦点,回车/空格确认,焦点与悬停共用一处高亮。
            let n = items.len();
            let mut list = col()
                .key("items")
                .role("listbox")
                .label(title.clone())
                .focus_group()
                .w_full()
                .gap(3.0);
            for (i, item) in items.iter().enumerate() {
                let label = item.clone();
                let picked = *action;
                let row = row()
                    .key(format!("item-{i}"))
                    .role("option")
                    .label(label.clone())
                    .position_in_set(i + 1, n)
                    .focusable(true)
                    .focus_selected(i == 0)
                    .focus_highlight(color(colors.hover))
                    .w_full()
                    .px(10.0)
                    .py(8.0)
                    .radius(8.0)
                    .on_press_tx(move |s: &mut State, _| pick(s, picked, i))
                    .child(
                        text(label)
                            .font(TextStyle::new(13.0, 400.0))
                            .color(color(colors.text))
                            .truncate(),
                    );
                // 弹层打开先落第一行;无 autofocus 也会落第一个 Tab 停靠点,写上更明确。
                list = list.child(if i == 0 { row.autofocus() } else { row });
            }
            (
                "picker",
                title.clone(),
                vec![
                    list.scroll(ScrollSpec {
                        stick_bottom: false,
                    })
                    .max_h(340.0),
                ],
            )
        }
        Overlay::Secret { title, .. } => (
            "secret",
            format!("扩展密钥 · {title}"),
            vec![
                text("只保存在本机;留空并保存即删除")
                    .font(TextStyle::new(11.0, 400.0))
                    .color(color(colors.text_secondary)),
                TextInput::new(&s.secret)
                    .width(None)
                    .secure()
                    .on_edit(|s: &mut State| &mut s.secret)
                    .build(kit),
            ],
        ),
        Overlay::Text { title, .. } => (
            "text",
            title.clone(),
            vec![
                TextArea::new(&s.text)
                    .width(None)
                    .max_height(320.0)
                    .on_edit(|s: &mut State| &mut s.text)
                    .build(kit),
            ],
        ),
        Overlay::Confirm { title, message, .. } => (
            "confirm",
            title.clone(),
            vec![
                text(message.clone())
                    .font(TextStyle::new(13.0, 400.0).lh(20.0))
                    .color(color(colors.text)),
            ],
        ),
    };
    let card = dialog_card(key, &title, body, overlay_actions(kit, overlay), colors);
    mount_overlay(key, card, layers);
}

/// 弹层底部的取消与确认/保存按钮。
fn overlay_actions(kit: &Kit, overlay: &Overlay) -> El {
    let cancel = KitButton::new("取消")
        .variant(ButtonVariant::Outline)
        .on_press(|s: &mut State| s.overlay = Overlay::None)
        .build(kit);
    let confirm = match overlay {
        Overlay::Picker { .. } | Overlay::Extension(_) | Overlay::None => el(),
        Overlay::Confirm { action, .. } => {
            let label = match action {
                ConfirmAction::Install => "安装",
                ConfirmAction::Consent { .. } => "同意",
            };
            KitButton::new(label)
                .variant(ButtonVariant::Contrast)
                .on_press(|s: &mut State| {
                    let Overlay::Confirm { action, .. } = &s.overlay else {
                        return;
                    };
                    let action = action.clone();
                    confirm(s, action);
                })
                .build(kit)
        }
        Overlay::Secret { ext, key, .. } => {
            let ext = ext.clone();
            let key = key.clone();
            KitButton::new("保存")
                .variant(ButtonVariant::Contrast)
                .on_press(move |s: &mut State| save_secret(s, ext.clone(), key.clone()))
                .build(kit)
        }
        Overlay::Text { path, kind, .. } => {
            let path = path.clone();
            let kind = *kind;
            KitButton::new("保存")
                .variant(ButtonVariant::Contrast)
                .on_press(move |s: &mut State| save_text(s, path.clone(), kind))
                .build(kit)
        }
    };
    row().gap(8.0).child(cancel).child(confirm)
}

fn dialog_card(key: &str, title: &str, body: Vec<El>, actions: El, colors: &Colors) -> El {
    let mut card = col()
        .key(format!("{key}-dialog"))
        .role("dialog")
        .label(title.to_string())
        .w_full()
        .max_w(480.0)
        .max_h(520.0)
        .padding(16.0)
        .gap(12.0)
        .radius(12.0)
        .fill(color(colors.surface))
        .border(1.0, color(colors.border))
        .modal(true, |s: &mut State| s.overlay = Overlay::None);
    card = card.child(
        text(title.to_string())
            .font(TextStyle::new(15.0, 600.0))
            .color(color(colors.text)),
    );
    for part in body {
        card = card.child(part);
    }
    card.child(actions)
}

fn mount_overlay(key: &str, card: El, layers: &mut Vec<El>) {
    layers.push(
        el().key(format!("{key}-scrim"))
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(Color::BLACK.alpha(0.42))
            .a11y_hidden(),
    );
    layers.push(
        col()
            .key(format!("{key}-layer"))
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .child(card),
    );
}

/// 自检:写报告并返回成败;首行必须是「结果:通过/失败」。
pub fn selftest(report: &Path) -> Result<(), String> {
    let mut lines: Vec<String> = Vec::new();
    let result = selftest_run(&mut lines);
    lines.push(match &result {
        Ok(()) => "结果:通过".to_string(),
        Err(error) => format!("结果:失败 — {error}"),
    });
    let _ = std::fs::write(report, lines.join("\n"));
    result
}

fn selftest_run(lines: &mut Vec<String>) -> Result<(), String> {
    let settings = Settings {
        current: update::current_version(),
        ..Settings::default()
    };
    let appearance = theme::Appearance::Light;
    let colors = theme::colors_with(appearance, 0);
    let pc = page_colors(&colors, appearance);
    let kit = Kit::from_palette(PaletteSeed {
        bg: color(colors.bg),
        fg: color(colors.text),
        accent: color(colors.accent),
    });
    let mut total = 0usize;
    let mut ids: Vec<String> = Vec::new();
    for page in PageId::ALL {
        let view = settings.page_view(page, &pc);
        let rows: usize = view.groups.iter().map(|group| group.rows.len()).sum();
        if rows == 0 {
            return Err(format!("页面 {:?} 一行都没有", page));
        }
        total += rows;
        ids.extend(view.nav.iter().map(|item| item.id.base.clone()));
        ids.extend(
            view.footer_buttons
                .iter()
                .map(|button| button.id.base.clone()),
        );
        if let Some(button) = &view.header_button {
            ids.push(button.id.base.clone());
        }
        for group in &view.groups {
            for row in &group.rows {
                control_ids(&row.control, &mut ids);
            }
        }
        // 每页构造一次控件树;panic 即视为链路断了。
        let _ = page_scene(&view, &kit, &colors);
        lines.push(format!("页面 {:?}: {rows} 行,控件树构造成功", page));
    }
    for want in [
        "nav-input",
        "nav-candidate",
        "nav-extensions",
        "nav-dictionary",
        "nav-about",
        "aux",
        "update",
        "phrases",
        "punct",
        "backup-export",
        "diagnostics",
        "close",
        "save",
    ] {
        if !ids.iter().any(|id| id == want) {
            return Err(format!("找不到关键控件 id: {want}"));
        }
    }
    lines.push(format!(
        "模型 {} 页共 {total} 行,关键控件 {} 个在位",
        PageId::ALL.len(),
        13
    ));
    let state = State::default();
    let _ = scene(&state);
    store::selftest()?;
    lines.push("扩展商店:点卡片、查看权限、启用与快捷键通过".to_string());
    Ok(())
}

fn control_ids(control: &Control, ids: &mut Vec<String>) {
    match control {
        Control::Toggle { id, .. }
        | Control::Segment { id, .. }
        | Control::Slider { id, .. }
        | Control::Preview { id, .. } => ids.push(id.base.clone()),
        Control::Buttons { items } | Control::ButtonsToggle { items, .. } => {
            ids.extend(items.iter().map(|button| button.id.base.clone()));
        }
        Control::None => {}
    }
}
