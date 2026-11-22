//! 输入法工具栏:一排常用按钮的悬浮条,Rune overlay 承载。
//!
//! 与候选窗、徽标同一 App、同一条线程;模式状态字(中英/全半/标点/简繁)
//! 在 poll 里对读全局状态与配置,有变化才重跑 view。位置与倍率持久化在
//! 配置里,拖动结束写回。按钮动作复用服务端已有的请求处理。位置、大小
//! 与按钮的模型在 `lueying_render::toolbar`,这里只负责窗口与交互。

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use lueying_protocol::Request;
use lueying_render::panel::Rect;
use lueying_render::toolbar::{self, Action, Button, Status, Toolbar};
use lueying_server::config;
use lueying_server::theme;
use rune::tok::motion;
use rune::{
    Cursor, Cx, Drag, El, KeyPolicy, OverlayHandle, OverlayInput, OverlayOptions, OverlaySpec,
    Phase, TextStyle, Tx, el, text,
};
use windows::Win32::Foundation::RECT;

use crate::imp::{State, handle};
use crate::ui::place;

/// 常驻的 overlay 句柄;别的线程靠它叫醒本线程。
static HANDLE: OnceLock<OverlayHandle<BarHud>> = OnceLock::new();

/// 工具栏 overlay 的状态;Rune 每帧从它重建控件树。
pub(super) struct BarHud {
    state: Arc<State>,
    /// 工具栏模型:可见性、倍率与位置;拖动数学也在里面。
    bar: Toolbar,
    /// 上次见到的配置版本;变了才重读倍率。
    version: u64,
    /// 上次见到的状态字;变了才重跑 view。
    status: Status,
    /// 外观与主题色下标按 poll 取一次:系统主题切得很慢,每帧读注册表纯属浪费。
    appearance: theme::Appearance,
    accent: usize,
    hover: Option<Button>,
    /// overlay 已显示;隐藏后 show 只在 poll 里发生。
    shown: bool,
    drag_moved: bool,
}

impl BarHud {
    fn new(state: Arc<State>, bar: Toolbar) -> Self {
        Self {
            status: status_of(&state),
            appearance: theme::Appearance::of_system(),
            accent: place::ui_options(&state).2,
            state,
            bar,
            version: 0,
            hover: None,
            shown: false,
            drag_moved: false,
        }
    }

    /// 按钮的动作。模式、全半角、标点复用服务端的请求处理;简繁写配置;
    /// 设置与收起直接处理。
    fn act(&mut self, button: Button) {
        match button.action() {
            Action::ToggleMode => {
                let session = self.state.active.load(Ordering::Relaxed);
                if session != 0 {
                    let _ = handle(&self.state, Request::ToggleMode { session });
                }
            }
            Action::ToggleFullWidth => {
                let session = self.state.active.load(Ordering::Relaxed);
                if session != 0 {
                    let _ = handle(&self.state, Request::ToggleFullwidth { session });
                }
            }
            Action::TogglePunct => {
                let _ = handle(&self.state, Request::TogglePunct);
            }
            Action::ToggleScript => toggle_traditional(&self.state),
            Action::OpenSettings => open_settings(),
            Action::Hide => self.bar.visible = false,
        }
    }
}

/// 装进候选窗 App:第三个 overlay,隐藏期不占窗口与表面。
pub(super) fn register<T: 'static>(app: &mut rune::App<T>, state: Arc<State>) {
    let bar = saved_bar(&state);
    let spec = OverlaySpec::new("lueying-toolbar", bar.size(), toolbar_view)
        .options(OverlayOptions {
            // 不吃按键;只有标了 hit 的条体拦指针,圆角外原生穿透。
            keys: KeyPolicy::None,
            input: OverlayInput::Regions,
            ..Default::default()
        })
        .release_gpu_when_hidden()
        .poll(toolbar_poll);
    let handle = app.overlay(spec, BarHud::new(state, bar));
    let _ = HANDLE.set(handle);
}

/// 配置或模式状态变了就叫醒工具栏;overlay 起来之前是空操作。
pub(super) fn kick() {
    if let Some(handle) = HANDLE.get() {
        handle.update(|_: &mut BarHud, _| {});
    }
}

/// 每帧开头跑一次:配置倍率、状态字、外观与可见性;有变化才让 view 重跑。
fn toolbar_poll(s: &mut BarHud, _now: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    let version = s.state.version.load(Ordering::Acquire);
    if version != s.version {
        s.version = version;
        let scale = {
            let cfg = s.state.config.read().unwrap_or_else(|p| p.into_inner());
            cfg.toolbar_scale
        };
        if (scale - s.bar.scale).abs() > f32::EPSILON {
            let work = place::work_area_at(s.bar.x as i32, s.bar.y as i32);
            s.bar = s.bar.with_scale(scale, to_rect(work));
            changed = true;
        }
    }
    let status = status_of(&s.state);
    if status != s.status {
        s.status = status;
        tx.animated();
        changed = true;
    }
    let appearance = theme::Appearance::of_system();
    let accent = place::ui_options(&s.state).2;
    if appearance != s.appearance || accent != s.accent {
        s.appearance = appearance;
        s.accent = accent;
        // 主题色与外观变化走官方的主题变化弹簧,颜色渐变过去。
        tx.color_motion(motion::CALM);
        changed = true;
    }
    if s.bar.visible {
        // 显示与任何几何变化都重新摆位;拖动自己也会即时摆位。
        if !s.shown || changed {
            place_bar(s);
        }
        if !s.shown {
            if let Some(handle) = HANDLE.get() {
                handle.show();
            }
            s.shown = true;
            changed = true;
        }
    } else if s.shown {
        if let Some(handle) = HANDLE.get() {
            handle.hide();
        }
        s.shown = false;
        changed = true;
    }
    changed
}

/// 摆位:模型的屏幕物理坐标换成 overlay placement(主屏本地逻辑坐标)。
fn place_bar(s: &BarHud) {
    let (w, h) = s.bar.size();
    let (x, y) = (s.bar.x as i32, s.bar.y as i32);
    let scale = place::scale_at(x, y);
    let rect = (x, y, (w * scale) as i32, (h * scale) as i32);
    if let Some(handle) = HANDLE.get() {
        handle.set_placement(Some(super::rune::placement_rect(rect, scale)));
    }
}

/// 一帧控件树:圆角条 + 一排按钮。
fn toolbar_view(s: &BarHud, cx: &mut Cx<BarHud>) -> El {
    // 系统关掉"动画效果"时,框架把过渡降级为淡出。
    cx.reduced_motion(!theme::client_area_animation());
    if !s.bar.visible {
        return el();
    }
    let t = toolbar::clamp_scale(s.bar.scale);
    let (w, h) = s.bar.size();
    let palette = theme::palette(theme::colors_with(s.appearance, s.accent));
    let mut bar = el()
        .w(w)
        .h(h)
        .radius(toolbar::RADIUS * t)
        .fill(theme::srgb(palette.bg))
        .border(1.0, theme::srgb(palette.border))
        // 条体是命中区:按下非按钮处开始拖动。
        .hit()
        .on_drag(|s: &mut BarHud, drag, _| on_bar_drag(s, drag));
    let size = toolbar::BUTTON * t;
    let gap = toolbar::GAP * t;
    for (index, button) in toolbar::BUTTONS.into_iter().enumerate() {
        bar = bar.child(button_el(
            s,
            button,
            index,
            gap + index as f32 * (size + gap),
            gap,
            size,
            t,
        ));
    }
    bar
}

/// 一个按钮:居中状态字,悬停亮底,按下执行动作。全半角按钮是月亮。
fn button_el(s: &BarHud, button: Button, index: usize, x: f32, y: f32, size: f32, t: f32) -> El {
    let palette = theme::palette(theme::colors_with(s.appearance, s.accent));
    let hovered = s.hover == Some(button);
    let mut cell = el()
        .absolute(x, y)
        .w(size)
        .h(size)
        .radius(size * 0.5)
        .center()
        .cursor(Cursor::Pointer)
        .role("button")
        .label(a11y_label(button))
        .key(format!("toolbar-{index}"))
        .on_hover_change(move |s: &mut BarHud, inside, _| {
            s.hover = inside.then_some(button);
        })
        .press_on_down()
        .on_press(move |s: &mut BarHud| s.act(button));
    cell = cell.child(
        text(toolbar::label(button, s.status))
            .font(TextStyle::new(toolbar::FONT * t, 400.0))
            .color(theme::srgb(palette.text)),
    );
    if hovered {
        cell = cell.fill(theme::srgb_alpha(palette.hover, 0.66));
    }
    cell
}

/// 拖动:按下非按钮处抓住,移动跟手并夹在屏幕内,松开写回配置。
/// 屏幕坐标用 `Drag::screen_point`(桌面物理像素),与模型、配置一致。
fn on_bar_drag(s: &mut BarHud, drag: &Drag) {
    let Some((sx, sy)) = drag.screen_point() else {
        return;
    };
    match drag.phase {
        Phase::Down => {
            s.bar.press(sx as f32, sy as f32);
            s.drag_moved = false;
        }
        Phase::Move => {
            let before = (s.bar.x, s.bar.y);
            let work = place::work_area_at(sx, sy);
            s.bar.drag(sx as f32, sy as f32, to_rect(work));
            if (s.bar.x, s.bar.y) != before {
                s.drag_moved = true;
                place_bar(s);
            }
        }
        Phase::Up => {
            s.bar.release();
            if s.drag_moved {
                save_position(&s.state, s.bar.x as i32, s.bar.y as i32);
            }
            s.drag_moved = false;
        }
    }
}

/// 上次保存的大小与位置;没有就用默认的右下角。位置夹在所在屏幕内。
fn saved_bar(state: &State) -> Toolbar {
    let (scale, pos) = {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        (cfg.toolbar_scale, cfg.toolbar_pos)
    };
    let screen = to_rect(place::primary_work_area());
    let mut bar = Toolbar::at_default(screen).with_scale(scale, screen);
    if let Some((x, y)) = pos {
        bar.x = x as f32;
        bar.y = y as f32;
        bar = bar.clamped(to_rect(place::work_area_at(x, y)));
    }
    bar
}

/// 拖动结束后把位置写回配置并保存,下次启动回到同一个地方。
fn save_position(state: &Arc<State>, x: i32, y: i32) {
    let (before, cfg) = {
        let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
        let before = cfg.clone();
        cfg.toolbar_pos = Some((x, y));
        (before, cfg.clone())
    };
    if let Some(path) = &state.config_path
        && let Err(e) = config::save(path, &before, &cfg)
    {
        println!("保存工具栏位置 {} 失败: {e}", path.display());
    }
}

/// 按钮上的字随的状态:来自服务端的全局状态与配置。
fn status_of(state: &State) -> Status {
    let (punct, traditional) = {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        (cfg.chinese_punct, cfg.traditional)
    };
    Status {
        chinese: state.chinese.load(Ordering::Relaxed),
        fullwidth: state.fullwidth.load(Ordering::Relaxed),
        punct,
        traditional,
    }
}

/// 简繁切换:写进配置并保存,然后应用。和标点切换走同一条路。
fn toggle_traditional(state: &Arc<State>) {
    let (before, cfg) = {
        let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
        let before = cfg.clone();
        cfg.traditional = !cfg.traditional;
        (before, cfg.clone())
    };
    match &state.config_path {
        Some(path) => {
            if let Err(e) = config::save(path, &before, &cfg) {
                println!("保存配置 {} 失败: {e}", path.display());
            }
        }
        None => println!("切换简繁未保存: 没有配置路径"),
    }
    crate::imp::apply_config(state, cfg);
}

/// 打开设置窗口:与语言栏的「设置」一样启动同目录的 `lueying-settings.exe`。
fn open_settings() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    if let Err(e) = std::process::Command::new(dir.join("lueying-settings.exe")).spawn() {
        println!("启动设置失败: {e}");
    }
}

/// 无障碍名:按钮只有状态字,读屏需要完整的动作说明。
fn a11y_label(button: Button) -> &'static str {
    match button {
        Button::Mode => "中英切换",
        Button::FullWidth => "全半角切换",
        Button::Punct => "中英文标点",
        Button::Script => "简繁切换",
        Button::Settings => "打开设置",
        Button::Hide => "收起工具栏",
    }
}

fn to_rect(r: RECT) -> Rect {
    Rect {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}
