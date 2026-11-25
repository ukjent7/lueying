//! 设置窗:Rune 承载页面,视觉与候选窗同一框架。页面模型在
//! `lueying_settings::settings::view`,这里负责窗口、控件与后台任务接线。
//!
//! 弹层(list / 多行文本 / 密钥 / 确认)都是主窗里的 scrim + 居中卡片,
//! 不开第二个 OS 窗口。`--selftest <报告路径>` 校验页面模型与控件树。

use std::cell::RefCell;
use std::path::Path;
use std::time::Duration;

use lueying_protocol::Request;
use rune::tok::motion;
use rune::{
    App, AppHandle, AppSpec, Color, Cursor, Cx, El, ScrollSpec, TextEdit, TextStyle,
    Tx, col, el, row, text,
};
use rune_kit::{
    Button as KitButton, ButtonVariant, ControlSize, Kit, PaletteSeed, SegmentedControl, Slider,
    Switch,
    TextArea, TextInput, root,
};

use crate::ext_secret;
use crate::settings::actions;
use crate::settings::model::{self as model, ButtonKind, Control};
use crate::settings::view::{Action, ConsentState, Notice, PageColors, PageId};
use crate::settings::view::{Settings, UpdateState};
use crate::update::{self, Check, Release};
use lueying_render::panel::{self, Estimate, Item, Layout, Metrics, PanelSpec};
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
    /// 系统外观缓存;None 时视图回退到即时读取(测试/未初始化)。
    appearance: Option<theme::Appearance>,
    /// 系统「减少动态效果」缓存;None 时视图回退到框架快照。
    reduced_motion: Option<bool>,
    /// 启动统计尚未落地;用户先导入词库时置 false,迟到的启动值不再覆盖。
    initial_groups_pending: bool,
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
    let mut state = State {
        core: load_core(),
        ..State::default()
    };
    // 真实窗口先填系统偏好;测试/缺省状态仍回退到框架快照。
    state.appearance = Some(theme::Appearance::of_system());
    state.reduced_motion = Some(!theme::client_area_animation());
    state.initial_groups_pending = true;
    let spec = AppSpec {
        families: ("Noto Sans SC", "Noto Sans SC"),
        min_size: Some((760.0, 480.0)),
        // 弹层全靠 rune 原生机制:卡片是 `.modal(...)`(Esc 关、点外关、焦点圈住),
        // 选择器是 `listbox` 焦点组(方向键巡游、回车/空格确认),无需 on_key。
        on_key: None,
        poll: Some(settings_poll),
        ..AppSpec::new("掠影设置", (960.0, 680.0), State::default, view)
    };
    let app = App::new(spec)
        .compositor(lueying_server::lighting::LightFx::lit_room([0.42, 0.42, 0.45]))
        .tab_visits_controls(true)
        .init(move || state);
    let handle = app.handle();
    HANDLE.with(|slot| *slot.borrow_mut() = Some(handle.clone()));
    // 首帧后再跑引擎状态与重排模型,首屏不被管道请求阻塞;Option 保证只一次。
    let mut startup = Some(handle);
    let app = app.on_ready(move |_| {
        if let Some(handle) = startup.take() {
            start_startup_load(handle);
        }
    });
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
    // 引擎状态由首帧后的后台线程读取,首屏不为两条管道请求阻塞。
    settings.status_loading = true;
    settings.current = update::current_version();
    settings.scripts = actions::installed_scripts();
    settings.ppocr_ready = actions::ppocr_ready();
    settings.ext_metrics = actions::read_ext_metrics();
    Core {
        settings,
        release: None,
    }
}


/// 窗顶天空的日/月。12 分钟一整圈(720 秒):前半圈太阳、后半圈
/// 月亮,各走正弦弧从左升右落。高度决定颜色与能量——日出日落
/// 变暖变暗(premultiplied fill 让 emission 自动跟暗),接近地平线
/// 时 alpha 渐隐。灯位由 compositor 每帧取自元素中心,零同步代码。
/// 造型对应样张 v2:太阳带日冕(自身大气,不是假光)、月亮带
/// 月坑(呼应工具栏),低 emission 增益保盘形。
fn sky_body() -> Vec<El> {
    const CYCLE: f64 = 720.0;
    const D: f32 = 26.0;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let cycle = (secs % CYCLE) / CYCLE; // 0..1
    let is_day = cycle < 0.5;
    let p = ((cycle % 0.5) * 2.0) as f32; // 半程内进度 0..1
    let elev = (std::f32::consts::PI * p).sin();
    // 行程:名义窗宽 960,从左 70 到右 864;地平线在 y 72,弧高 40。
    let x = 70.0 + p * (864.0 - 70.0) - D / 2.0;
    let y = 72.0 - elev * 40.0 - D / 2.0;
    let fade = (elev * 3.0).clamp(0.0, 1.0);

    let hex = if is_day {
        // 太阳:低位橙(0xff9a50)→高位亮黄白(0xffe9a0)。
        let t = elev.powf(0.7);
        let ch = |shift: u32| -> u32 {
            let ca = ((0xff9a50u32 >> shift) & 0xff) as f32;
            let cb = ((0xffe9a0u32 >> shift) & 0xff) as f32;
            (ca + (cb - ca) * t) as u32
        };
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    } else {
        0xdfe8f5 // 月亮:冷白,与太阳拉开色温
    };

    let mut sky: Vec<El> = Vec::new();
    if is_day {
        // 日冕:太阳自身的大气,radial 渐变衰减到透明——不是照明贴图,
        // 是天体外观的一部分。
        let cd = D * 1.9;
        let cx0 = x + D / 2.0 - cd / 2.0;
        let cy0 = y + D / 2.0 - cd / 2.0;
        sky.push(
            el().absolute(cx0, cy0)
                .w(cd)
                .h(cd)
                .radius(cd / 2.0)
                .radial_gradient(
                    [0.5, 0.5],
                    cd / 2.0,
                    [
                        (0.0, color(hex).alpha(0.35 * fade)),
                        (0.55, color(hex).alpha(0.12 * fade)),
                        (1.0, color(hex).alpha(0.0)),
                    ],
                )
                .key("sun-corona")
                .a11y_hidden(),
        );
    }
    let disc = el()
        .absolute(x, y)
        .w(D)
        .h(D)
        .radius(D / 2.0)
        .fill(color(hex).alpha(fade))
        .key(if is_day { "sun" } else { "moon" })
        .mark(lueying_server::lighting::MARK_EMITTER_SKY)
        .a11y_hidden();
    sky.push(disc);
    if !is_day {
        // 月坑三粒,比例照抄工具栏月亮:两个月亮是同一个月亮。
        // 坑在盘之上(后画),全部绝对坐标平铺,不套层。
        for (fx, fy, fd) in [(0.66f32, 0.70f32, 0.17f32), (0.76, 0.45, 0.12), (0.56, 0.80, 0.09)] {
            let d = fd * D;
            sky.push(
                el().absolute(x + (fx - fd / 2.0) * D, y + (fy - fd / 2.0) * D)
                    .w(d)
                    .h(d)
                    .radius(d / 2.0)
                    .fill(color(0x9aa4b8).alpha(0.4 * fade))
                    .key("crater")
                    .a11y_hidden(),
            );
        }
    }
    sky
}

fn view(s: &State, cx: &mut Cx<State>) -> El {
    // 系统关掉"动画效果"时,框架把弹簧动效降级为淡出;真实窗口用
    // poll 缓存的运行时值,默认/测试状态回退到宿主快照。
    let reduced = s
        .reduced_motion
        .unwrap_or_else(|| cx.system_reduced_motion());
    cx.reduced_motion(reduced);
    scene(s)
}

/// 前台每秒复查系统外观与「减少动态效果」;隐藏时不读注册表,也不
/// 常驻唤醒。缓存值真的变了才返回 true,不改焦点、弹层或黑瓷身份。
fn settings_poll(s: &mut State, _now: f64, tx: &mut Tx) -> bool {
    if !tx.window_visible() {
        return false;
    }
    let appearance = theme::Appearance::of_system();
    let reduced = !theme::client_area_animation();
    let mut changed = false;
    if s.appearance != Some(appearance) {
        s.appearance = Some(appearance);
        changed = true;
    }
    if s.reduced_motion != Some(reduced) {
        s.reduced_motion = Some(reduced);
        changed = true;
    }
    tx.wake_after(Duration::from_millis(500));
    changed
}

/// 0xRRGGBB 转线性 sRGB 颜色;转换共享一份,见 `lueying_server::theme::srgb`。
fn color(rgb: u32) -> Color {
    theme::srgb(rgb)
}

/// 页面需要的提示色与主题色选项,顺序同 `ACCENTS`。设置窗固定黑瓷:
/// 普通外观下列出与黑瓷配套的 dark 预设;系统高对比时用系统 accent。
fn page_colors(c: &Colors, appearance: theme::Appearance) -> PageColors {
    PageColors {
        notice_info: c.text_secondary,
        notice_success: c.success,
        notice_error: c.danger,
        accents: ACCENTS
            .iter()
            .map(|accent| match appearance {
                theme::Appearance::HighContrast => c.accent,
                _ => accent.dark[0],
            })
            .collect(),
    }
}

fn scene(s: &State) -> El {
    // 外观由 poll 缓存;高对比度让位系统色,普通深浅仍固定中性黑瓷。
    let appearance = s.appearance.unwrap_or_else(theme::Appearance::of_system);
    // 固定中性黑瓷:瓷是身份材质,不跟系统深浅或候选窗材质走;
    // 高对比度让位系统色。
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
        .h(58.0)
        .px(18.0)
        .gap(16.0)
        .center_y()
        .child(
            col()
                .key("titles")
                .grow(1.0)
                .min_w(0.0)
                .gap(1.0)
                .child(
                    text(page.title.clone())
                        .font(TextStyle::new(21.0, 600.0).lh(25.0))
                        .color(color(colors.text))
                        .truncate(),
                )
                .child(
                    text(page.subtitle.clone())
                        .font(TextStyle::new(11.0, 400.0))
                        .color(color(colors.text_secondary))
                        .truncate(),
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
                .focus_ring(2.0, -2.0, color(colors.accent_text))
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
                            colors.selected_text
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
        .child(page_scene(&page, &kit, &colors, appearance));

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
        footer = footer.child(footer_button_el(button, &kit));
    }

    let mut base = col()
        .key("window")
        .role("window")
        .w_full()
        .h_full()
        .fill(color(colors.bg));
    // 黑瓷受光面:非高对比时标记身份,光照 compositor 按它打光。
    if !matches!(appearance, theme::Appearance::HighContrast) {
        base = base.mark(lueying_server::lighting::MARK_SHELL);
    }
    // 天空:日月沿窗顶横越,自身就是 emitter——光从你看得见的
    // 地方来。12 分钟一圈的设计节奏,不是真实时间。元素平铺进
    // base,不套零尺寸容器(裁切不确定)。
    if !matches!(appearance, theme::Appearance::HighContrast) {
        for body in sky_body() {
            base = base.child(body);
        }
    }
    base = base.child(header).child(body).child(footer);

    let mut layers = Vec::new();
    overlay_scene(
        s,
        &s.overlay,
        &kit,
        &colors,
        &page_colors,
        appearance,
        &mut layers,
    );
    root(base, layers)
}

/// 右栏:滚动的卡片流。
fn page_scene(page: &model::Page, kit: &Kit, colors: &Colors, appearance: theme::Appearance) -> El {
    let mut cards = col().key("cards").w_full().gap(12.0);
    // 相邻同名分组只画一次标题,官方扩展的多张卡片不重复起头。
    let mut previous_title = None;
    for group in &page.groups {
        let show_title = previous_title != Some(group.title.as_str());
        cards = cards.child(group_card(group, kit, colors, appearance, show_title));
        previous_title = Some(group.title.as_str());
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
        .scroll_margin(18.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(cards)
}

fn group_card(
    group: &model::Group,
    kit: &Kit,
    colors: &Colors,
    appearance: theme::Appearance,
    show_title: bool,
) -> El {
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
    // 卡片铺在不透黑瓷窗底上:层级由表面色与结构描边承担,不再从
    // 窗底色伪造一层环境釉光;真实光只来自界面发光体本身。
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
    let mut inner = col().w_full().padding(10.0);
    for (i, row_data) in group.rows.iter().enumerate() {
        inner = inner.child(setting_row(row_data, kit, colors, appearance).key(format!("row-{i}")));
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
    let mut section = col().key(format!("section-{key}")).w_full().gap(6.0);
    if show_title {
        section = section.child(
            text(group.title.clone())
                .font(TextStyle::new(12.0, 600.0))
                .color(color(colors.text_secondary))
                .ml(4.0),
        );
    }
    section.child(card)
}

fn setting_row(
    row_data: &model::Row,
    kit: &Kit,
    colors: &Colors,
    appearance: theme::Appearance,
) -> El {
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
                    .wrap(),
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
            .child(candidate_preview(style, colors, appearance)),
        _ => row()
            .key("setting-row")
            .w_full()
            .min_h(52.0)
            .px(10.0)
            .py(5.0)
            .gap(12.0)
            .center_y()
            .child(info())
            .child(control_el(
                &row_data.control,
                kit,
                colors,
                &row_data.label,
                appearance,
            )),
    }
}

/// 候选窗预览:用真实 panel 布局量出固定条/竖排的每条矩形与圆角;
/// 预览按 0.9 缩放画,最小设置宽度也留得下横排,竖排预算更窄。
/// 不画可点的条尾箭头——结构性预览不是交互件,模型已留了同样的尾宽。
fn candidate_preview(
    style: &model::CandidatePreview,
    colors: &Colors,
    appearance: theme::Appearance,
) -> El {
    let metrics = Metrics::scaled(style.font_scale * 0.9);
    let scale = metrics.scale();
    let max_text_width = 380.0 * scale;
    let spec = PanelSpec {
        width: 0.0,
        max_text_width,
        expand: false,
        items: style
            .candidates
            .iter()
            .take(style.page_size.clamp(1, 9))
            .enumerate()
            .map(|(i, candidate)| Item {
                text: panel::ellipsize(candidate, max_text_width, metrics.text, &Estimate),
                hint: String::new(),
                selected: i == 0,
            })
            .collect(),
        grid: Vec::new(),
        scroll: None,
        layout: if style.vertical {
            Layout::Column
        } else {
            Layout::Wrap
        },
    };
    let (w, h) = (
        panel::panel_width(&spec, &metrics, &Estimate),
        panel::panel_height(&spec, &metrics, &Estimate),
    );
    let radius = panel::surface_radius(spec.is_strip(), w, h, scale);
    let high_contrast = matches!(appearance, theme::Appearance::HighContrast);
    let (selected_fill, selected_ink) = if high_contrast {
        (color(colors.selected_bg), color(colors.selected_text))
    } else {
        theme::capsule_colors(style.accent)
    };
    let mut surface = el()
        .key("candidate-preview")
        .w(w)
        .h(h)
        .radius(radius)
        .continuous_corners()
        .clip_rounded(radius)
        .fill(color(colors.surface))
        .border(1.0, color(colors.border));
    for (i, (item, rect)) in spec
        .items
        .iter()
        .zip(panel::item_rects(&spec, &metrics, &Estimate))
        .enumerate()
    {
        let mut cell = if item.selected {
            lueying_server::capsule::selection_capsule(rect, scale, selected_fill, high_contrast)
                .key(format!("candidate-{i}"))
        } else {
            el().absolute(rect.left, rect.top)
                .w(rect.width())
                .h(rect.height())
                .key(format!("candidate-{i}"))
        };
        cell = cell
            .child(
                text(format!("{}", i + 1))
                    .absolute(
                        panel::ITEM_PAD * scale,
                        (rect.height() - metrics.small) * 0.5,
                    )
                    .font(TextStyle::new(metrics.small, 400.0))
                    .color(if item.selected {
                        selected_ink
                    } else {
                        color(colors.text_tertiary)
                    }),
            )
            .child(
                text(item.text.clone())
                    .absolute(panel::TEXT_X * scale, (rect.height() - metrics.text) * 0.5)
                    .w((rect.width() - (panel::TEXT_X + panel::ITEM_PAD) * scale).max(1.0))
                    .font(TextStyle::new(metrics.text, 400.0))
                    .color(if item.selected {
                        selected_ink
                    } else {
                        color(colors.text)
                    })
                    .truncate(),
            );
        surface = surface.child(cell);
    }
    surface
}

fn control_el(
    control: &Control,
    kit: &Kit,
    colors: &Colors,
    label: &str,
    appearance: theme::Appearance,
) -> El {
    match control {
        Control::Toggle { id, on } => {
            let base = id.base.clone();
            Switch::new(*on)
                .label(label)
                .hide_label()
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
            let group = SegmentedControl::new(&labels, Some(*selected)).label(label);
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
        Control::Slider { id, value } => {
            let base = id.base.clone();
            Slider::new(format!("slider-{}", id.base), *value)
                .range(0.0..=1.0)
                .step(0.05)
                .label(label)
                .hide_label()
                .width(180.0)
                .value_text(format!("{:.0}%", value * 100.0))
                .on_change(move |s: &mut State, v, _done| {
                    s.core.settings.set_slider(&base, v);
                })
                .build(kit)
        }
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
                        .label(label)
                        .hide_label()
                        .on_toggle(move |s: &mut State| activate(s, base.clone(), 0))
                        .build(kit),
                )
        }
        Control::Preview { style, .. } => candidate_preview(style, colors, appearance),
        Control::None => el(),
    }
}

/// 滑杆:轨道 + 左侧强调色高亮段 + 圆形拇指;拖动即改值。
/// 取值 0..=1,保存按钮把整份配置落盘。写法对齐官方 settings 示例的滑杆:
/// a11y 走 role/range_value/on_step(方向键 0.05 步进),按下记值、Esc 取消恢复,
/// 点在轨道远处用弹簧滑过去,拖动中 tx.instant() 让拇指严格跟手。
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

/// 页脚按钮:回车只在它自己聚焦时激活,不设全局默认按钮。
fn footer_button_el(button: &model::Button, kit: &Kit) -> El {
    let base = button.id.base.clone();
    let index = button.id.index;
    let control = button_el(button, kit).key(format!("footer-{base}"));
    if !button.enabled {
        return control;
    }
    control.on_key(move |s: &mut State, event, tx| {
        if event.key != rune::Key::Enter || event.mods != rune::Mods::default() {
            return false;
        }
        if !tx.key_repeat() {
            activate(s, base.clone(), index);
        }
        true
    })
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
        Action::ImportUser => {
            if actions::import_user(&mut s.core.settings) {
                s.initial_groups_pending = false;
            }
        }
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
    // 短语文件可以上百 KB,读盘不占 UI 帧;结果回 UI 线程再开编辑层。
    let Some(handle) = ui_handle() else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("settings-text".into())
        .spawn(move || {
            let body = std::fs::read_to_string(&path);
            handle.update(move |s: &mut State, _| match body {
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
            });
        });
    if let Err(error) = spawned {
        s.core
            .settings
            .say(Notice::Error, format!("读文件线程启动失败: {error}"));
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

/// 首帧后的后台加载:先读引擎状态与模糊组,再补齐重排模型,最后再读
/// 一次状态。管道请求全在工作线程完成,两个 update 只搬数据,保持
/// 初始/刷新顺序,不覆盖用户已改的配置、页面或搜索。
fn start_startup_load(handle: AppHandle<State>) {
    let fallback = handle.clone();
    let spawned = std::thread::Builder::new()
        .name("settings-startup".into())
        .spawn(move || {
            let groups = actions::call(&Request::FuzzyStats)
                .map(|reply| reply.groups)
                .unwrap_or_default();
            let status = actions::call(&Request::Status).and_then(|reply| reply.status);
            handle.update(move |s: &mut State, _| {
                if std::mem::take(&mut s.initial_groups_pending) {
                    s.core.settings.groups = groups;
                }
                s.core.settings.status = status;
                s.core.settings.status_loading = false;
            });

            let result = crate::models::ensure();
            if result.as_ref().is_ok_and(|&count| count > 0) {
                actions::restart_server();
            }
            let status = actions::call(&Request::Status).and_then(|reply| reply.status);
            handle.update(move |s: &mut State, _| {
                s.core.settings.status = status;
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
        eprintln!("设置后台加载线程启动失败: {error}");
        fallback.update(|s: &mut State, _| {
            s.initial_groups_pending = false;
            s.core.settings.status_loading = false;
        });
    }
}
/// 主窗内的弹层:全窗 scrim + 居中卡片,四种弹层共用。
fn overlay_scene(
    s: &State,
    overlay: &Overlay,
    kit: &Kit,
    colors: &Colors,
    prefs: &PageColors,
    appearance: theme::Appearance,
    layers: &mut Vec<El>,
) {
    let (key, title, body): (&str, String, Vec<El>) = match overlay {
        Overlay::None => return,
        Overlay::Extension(id) => {
            store::detail(s, id, kit, colors, prefs, appearance, layers);
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
    // 高对比用不透明系统窗口色遮住背景,普通黑瓷保留原遮挡层。
    let scrim = if matches!(appearance, theme::Appearance::HighContrast) {
        color(colors.bg)
    } else {
        Color::BLACK.alpha(0.42)
    };
    mount_overlay(key, card, layers, scrim);
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

fn mount_overlay(key: &str, card: El, layers: &mut Vec<El>, scrim: Color) {
    layers.push(
        el().key(format!("{key}-scrim"))
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .fill(scrim)
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

/// 调试快照:独立进程渲染真实候选外观设置页并写 PNG,不读配置、不开窗口、
/// 不接管道。default 常规宽度,stress 窄窗 + 竖排 + 大字号 + 9 候选。
#[cfg(debug_assertions)]
pub fn snap_candidate(stress: bool, output: String) -> Result<(), String> {
    let mut state = State {
        appearance: Some(theme::Appearance::Dark),
        reduced_motion: Some(true),
        ..State::default()
    };
    state.core.settings.page = PageId::Candidate;
    let size = if stress {
        (760.0, 800.0)
    } else {
        (960.0, 680.0)
    };
    if stress {
        state.core.settings.cfg.ui_layout = config::UiLayout::Vertical;
        state.core.settings.cfg.ui_font = config::UiFont::Large;
        state.core.settings.cfg.page_size = 9;
    }
    snap_state(state, size, output)
}

#[cfg(debug_assertions)]
fn snap_state(state: State, size: (f32, f32), output: String) -> Result<(), String> {
    let spec = AppSpec {
        families: ("Noto Sans SC", "Noto Sans SC"),
        ..AppSpec::new("Lueying Settings Snapshot", size, State::default, view)
    };
    // 快照必须与真实应用同构:光照 compositor 缺失会让设置页
    // 渲染退化成无光照平底,验证失真。
    App::new(spec)
        .compositor(lueying_server::lighting::LightFx::lit_room([0.42, 0.42, 0.45]))
        .tab_visits_controls(true)
        .init(move || state)
        .args([
            "--snap".into(),
            output,
            "--scale".into(),
            "1".into(),
            "--at".into(),
            "0.35".into(),
        ])
        .try_run()
}

/// 调试快照:渲染指定设置页,统一用最小支持窗口宽度。
#[cfg(debug_assertions)]
pub fn snap_page(scene: &str, output: String) -> Result<(), String> {
    let page = match scene {
        "input" => PageId::Input,
        "candidate" => PageId::Candidate,
        "extensions" => PageId::Extensions,
        "dictionary" => PageId::Dictionary,
        "about" => PageId::About,
        "search" => PageId::Input,
        _ => return Err(format!("未知场景: {scene}")),
    };
    let mut state = State {
        appearance: Some(theme::Appearance::Dark),
        reduced_motion: Some(true),
        ..State::default()
    };
    state.core.settings.page = page;
    state.core.settings.current = update::current_version();
    if scene == "search" {
        state.search = TextEdit::new("候选".to_owned());
        state.core.settings.set_search("候选".to_owned());
    }
    snap_state(state, (760.0, 480.0), output)
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
    // 自检按实际设置窗材质构建:固定中性黑瓷,不跟系统深浅走。
    let appearance = theme::Appearance::Dark;
    let colors = theme::ceramic_colors();
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
        let _ = page_scene(&view, &kit, &colors, appearance);
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
    keyboard_selftest(lines)?;
    store::selftest()?;
    lines.push("扩展商店:点卡片、查看权限、启用与快捷键通过".to_string());
    Ok(())
}

/// 键盘旅程:真实 headless Runtime 走一遍搜索、导航、页内滚动、开关与页脚。
fn keyboard_selftest(lines: &mut Vec<String>) -> Result<(), String> {
    fn focus_desc(el: &rune::ElementInfo) -> String {
        el.key
            .clone()
            .or_else(|| el.label.clone())
            .unwrap_or_else(|| el.role.to_string())
    }
    fn focused(rt: &rune::Runtime<State>) -> Option<rune::ElementInfo> {
        rt.elements()
            .into_iter()
            .find(|el| el.focused && el.focus_visible)
    }
    fn predict_switch(rt: &rune::Runtime<State>) -> Option<rune::ElementInfo> {
        rt.elements()
            .into_iter()
            .find(|el| el.role == "switch" && el.label.as_deref() == Some("下一词联想"))
    }

    let state = State {
        appearance: Some(theme::Appearance::Dark),
        reduced_motion: Some(true),
        ..State::default()
    };
    let mut rt = rune::Runtime::new(state, view, (760.0, 480.0), rune::Env::new(true, 1.0));
    // 系统「全键盘控制」关闭时,应用的 Tab 巡游仍要覆盖所有控件。
    rt.set_full_keyboard_access(false);
    rt.set_tab_visits_controls(true);
    rt.frame(0.0);
    let key = |rt: &mut rune::Runtime<State>, key: rune::Key, shift: bool| {
        rt.key(rune::KeyEvent {
            key,
            mods: rune::Mods {
                shift,
                ..rune::Mods::default()
            },
        });
        rt.frame(0.0);
    };

    // a. 首个 Tab 到搜索框。
    key(&mut rt, rune::Key::Tab, false);
    let at_search = focused(&rt).ok_or_else(|| "键盘: Tab 后没有可见焦点".to_string())?;
    if at_search.key.as_deref() != Some("search") && at_search.label.as_deref() != Some("搜索设置")
    {
        return Err(format!(
            "键盘: 首个 Tab 应到搜索框,实际 key={:?} label={:?} role={}",
            at_search.key, at_search.label, at_search.role
        ));
    }
    lines.push(format!(
        "键盘: 首个 Tab 停在搜索框(key={:?}, label={:?})",
        at_search.key, at_search.label
    ));

    // b. 搜索框里回车不能落到保存。
    key(&mut rt, rune::Key::Enter, false);
    let settings = &rt.state.core.settings;
    if settings.notice == Notice::Error {
        return Err(format!(
            "键盘: 搜索框回车触发了保存/错误提示: {}",
            settings.message
        ));
    }
    lines.push(format!(
        "键盘: 搜索框回车不保存、无错误(notice={:?})",
        settings.notice
    ));

    // c. 左栏:Tab 到输入分类,Down 到候选,Enter 切页。
    key(&mut rt, rune::Key::Tab, false);
    let nav = focused(&rt).ok_or_else(|| "键盘: Tab 后没有可见焦点".to_string())?;
    if nav.key.as_deref() != Some("nav-nav-input") {
        return Err(format!(
            "键盘: Tab 应到左侧「输入」,实际 key={:?} label={:?}",
            nav.key, nav.label
        ));
    }
    key(&mut rt, rune::Key::Down, false);
    let nav = focused(&rt).ok_or_else(|| "键盘: Down 后没有可见焦点".to_string())?;
    if nav.key.as_deref() != Some("nav-nav-candidate") {
        return Err(format!(
            "键盘: Down 应到左侧「候选」,实际 key={:?} label={:?}",
            nav.key, nav.label
        ));
    }
    key(&mut rt, rune::Key::Enter, false);
    if rt.state.core.settings.page != PageId::Candidate {
        return Err(format!(
            "键盘: 在左侧「候选」回车没有切页,实际 {:?}",
            rt.state.core.settings.page
        ));
    }
    lines.push("键盘: Tab→nav-input,Down→nav-candidate,Enter 切到候选页".into());

    // d. 「下一词联想」初始被页底裁掉;Tab 走到它时页要滚动并留出边距。
    let predict =
        predict_switch(&rt).ok_or_else(|| "键盘: 候选页找不到「下一词联想」开关".to_string())?;
    let (rect, visible) = (predict.rect, predict.visible);
    if !(visible[3] < rect[3] - 1.0) {
        return Err(format!(
            "键盘: 「下一词联想」初始应被页底裁掉,rect={rect:?} visible={visible:?}"
        ));
    }
    lines.push(format!(
        "键盘: 「下一词联想」初始在视口外(rect y={:.1}..{:.1}, visible y={:.1}..{:.1})",
        rect[1],
        rect[1] + rect[3],
        visible[1],
        visible[1] + visible[3]
    ));

    let mut tab_count = 0usize;
    let mut reached = Vec::new();
    loop {
        if tab_count == 24 {
            return Err(format!(
                "键盘: Tab 24 次仍未到「下一词联想」;途经 {:?}",
                reached
            ));
        }
        key(&mut rt, rune::Key::Tab, false);
        tab_count += 1;
        let el =
            focused(&rt).ok_or_else(|| format!("键盘: 第 {tab_count} 次 Tab 后没有可见焦点"))?;
        reached.push(focus_desc(&el));
        if el.role == "switch" && el.label.as_deref() == Some("下一词联想") {
            break;
        }
    }
    let predict =
        predict_switch(&rt).ok_or_else(|| "键盘: 聚焦后找不到「下一词联想」开关".to_string())?;
    let (rect, visible) = (predict.rect, predict.visible);
    if (visible[2] - rect[2]).abs() > 1.0 || (visible[3] - rect[3]).abs() > 1.0 {
        return Err(format!(
            "键盘: Tab 到「下一词联想」后仍被裁,rect={rect:?} visible={visible:?} scrolled={:?}",
            predict.scrolled
        ));
    }
    if !(predict.scrolled[1] > 0.0) {
        return Err(format!(
            "键盘: Tab 到「下一词联想」后页面没有滚动,scrolled={:?} rect={rect:?} visible={visible:?}",
            predict.scrolled
        ));
    }
    lines.push(format!(
        "键盘: Tab {tab_count} 次到「下一词联想」({}),整行可见并滚动 {:.1}pt;途经 {:?}",
        focus_desc(&predict),
        predict.scrolled[1],
        reached
    ));

    // e. 空格翻转开关。
    let before = rt.state.core.settings.cfg.predict;
    key(&mut rt, rune::Key::Text(" ".into()), false);
    let after = rt.state.core.settings.cfg.predict;
    if after == before {
        return Err(format!(
            "键盘: 空格没有翻转「下一词联想」({before} -> {after}),当前焦点 {:?}",
            focused(&rt).as_ref().map(focus_desc)
        ));
    }
    lines.push(format!("键盘: 空格翻转「下一词联想」{before} -> {after}"));

    // f. 页脚顺序:关闭、保存;Shift+Tab 回关闭,再 Tab 回保存。
    let expect_focus =
        |rt: &mut rune::Runtime<State>, want: &str, shift: bool| -> Result<(), String> {
            key(rt, rune::Key::Tab, shift);
            let el = focused(rt).ok_or_else(|| {
                format!(
                    "键盘: {}Tab 后没有可见焦点",
                    if shift { "Shift+" } else { "" }
                )
            })?;
            if el.key.as_deref() != Some(want) {
                return Err(format!(
                    "键盘: {}Tab 应到 {want},实际 key={:?} label={:?}",
                    if shift { "Shift+" } else { "" },
                    el.key,
                    el.label
                ));
            }
            Ok(())
        };
    expect_focus(&mut rt, "footer-close", false)?;
    expect_focus(&mut rt, "footer-save", false)?;
    expect_focus(&mut rt, "footer-close", true)?;
    expect_focus(&mut rt, "footer-save", false)?;
    lines.push("键盘: 页脚 Tab→关闭→保存,Shift+Tab→关闭,Tab→保存".into());

    // g. 保存按钮的回车走真实回调;无配置路径时只报错,不写文件。
    key(&mut rt, rune::Key::Enter, false);
    let settings = &rt.state.core.settings;
    if settings.notice != Notice::Error || settings.message != "没有可写的配置路径" {
        return Err(format!(
            "键盘: 保存回车没有执行真实回调: notice={:?} message={:?}",
            settings.notice, settings.message
        ));
    }
    lines.push(format!(
        "键盘: 保存回车触发真实回调 -> Error「{}」(未写文件)",
        settings.message
    ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    /// 行标题由设置行渲染;开关与分段组只把它作为无障碍标签,不再画第二份。
    fn view(_: &State, _: &mut Cx<State>) -> El {
        let colors = theme::ceramic_colors();
        let kit = Kit::from_palette(PaletteSeed {
            bg: color(colors.bg),
            fg: color(colors.text),
            accent: color(colors.accent),
        });
        let toggle = Control::Toggle {
            id: model::Id::new("fixture-toggle", 0),
            on: true,
        };
        let segment = Control::Segment {
            id: model::Id::new("fixture-segment", 0),
            options: vec![
                model::SegmentOption::new("标准"),
                model::SegmentOption::new("紧凑"),
            ],
            selected: 0,
            cell_width: None,
        };
        col()
            .gap(8.0)
            .child(control_el(
                &toggle,
                &kit,
                &colors,
                "候选窗动画",
                theme::Appearance::Dark,
            ))
            .child(control_el(
                &segment,
                &kit,
                &colors,
                "候选窗排列",
                theme::Appearance::Dark,
            ))
    }

    #[test]
    fn switch_and_segment_nodes_carry_the_row_label() {
        let mut rt = Runtime::new(State::default(), view, (480.0, 160.0), Env::new(true, 1.0));
        rt.frame(0.0);
        let elements = rt.elements();
        let switch = elements
            .iter()
            .find(|element| element.role == "switch")
            .expect("开关节点");
        assert_eq!(switch.label.as_deref(), Some("候选窗动画"));
        let group = elements
            .iter()
            .find(|element| element.role == "radiogroup")
            .expect("分段组节点");
        assert_eq!(group.label.as_deref(), Some("候选窗排列"));
    }
}
