//! 输入法工具栏:一排常用按钮的悬浮条,Rune overlay 承载。
//!
//! 与候选窗、徽标同一 App、同一条线程;模式状态字(中英/全半/标点/简繁)
//! 在 poll 里对读全局状态与配置,有变化才重跑 view。位置与倍率持久化在
//! 配置里,拖动结束写回。按钮动作复用服务端已有的请求处理。位置、大小
//! 与按钮的模型在 `lueying_render::toolbar`,这里只负责窗口与交互。

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use lueying_protocol::Request;
use lueying_render::panel::{Palette, Rect};
use lueying_render::toolbar::{self, Action, Button, Status, Toolbar};
use lueying_server::config;
use lueying_server::theme;
use rune::tok::motion;
use rune::{
    Color, Cursor, Cx, Drag, El, KeyPolicy, OverlayHandle, OverlayInput, OverlayOptions,
    OverlaySpec, Phase, Shape, TextStyle, Tx, canvas, el, text,
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
    /// 月相 0=弯月 1=满月;形变期间由 poll 里的弹簧推进,canvas 路径按它重建。
    phase: f32,
    /// 月相弹簧的初值与起点时刻(帧钟秒);目标翻转时记下当前相位接力。
    phase_from: (f32, f64),
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
        let status = status_of(&state);
        Self {
            // 启动即落在当前月相,首帧不播形变。
            phase: if status.fullwidth { 1.0 } else { 0.0 },
            phase_from: (if status.fullwidth { 1.0 } else { 0.0 }, 0.0),
            status,
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
                // active 为 0(焦点落在没有掠影会话的窗口)也照发:
                // 分发端没有会话就只翻全局位,按钮不能静默无效。
                let session = self.state.active.load(Ordering::Relaxed);
                let _ = handle(&self.state, Request::ToggleMode { session });
            }
            Action::ToggleFullWidth => {
                let session = self.state.active.load(Ordering::Relaxed);
                let _ = handle(&self.state, Request::ToggleFullwidth { session });
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
        .poll(toolbar_poll)
        // 事件循环一起就走一遍既有的 kick 路径(排空更新→首轮 poll→show);
        // 不然工具栏要等到第一次模式切换或设置保存才出现(升级装完服务
        // 重启后没有任何 kick,打字只叫醒候选窗)。
        .setup(|_: &mut BarHud, _| kick());
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
fn toolbar_poll(s: &mut BarHud, now: f64, tx: &mut Tx) -> bool {
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
        // 全/半切换是月亮的形变:下面的月相弹簧把暗面侵蚀到位。
        s.phase_from = (s.phase, now);
        tx.animated();
        changed = true;
    }
    // 月相弹簧:0=弯月,1=满月;闭式弹簧按经过时间采样,形变期间
    // 每帧重采样并叫醒下一拍(shapes 示例的 poll 驱动同款),到位即停。
    // 系统关掉动画效果就直落。
    let target = if s.status.fullwidth { 1.0 } else { 0.0 };
    if s.phase != target {
        if theme::client_area_animation() {
            let t = (now - s.phase_from.1) as f32;
            let (p, v) = rune::Spring::new(0.28, 0.12).sample(s.phase_from.0, 0.0, target, t);
            s.phase = p;
            // 位置与速度都近零才算停;采样值过冲是弹簧的自然回弹。
            if (s.phase - target).abs() < 0.005 && v.abs() < 0.05 {
                s.phase = target;
            } else {
                tx.wake_after(Duration::ZERO);
            }
        } else {
            s.phase = target;
        }
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
    // 系统关掉"动画效果"时,框架把过渡降级为淡出;上游宿主喂的
    // 就是同一个 SPI,不重复读注册表。
    cx.reduced_motion(cx.system_reduced_motion());
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
        bar = bar.child(button_el(s, button, index, size, gap, t, palette));
    }
    bar
}

/// 一个按钮:居中状态字,悬停亮底,按下执行动作。全半角按钮是月亮。
/// `palette` 由 `toolbar_view` 算一次传下来,每个按钮不重复取色;
/// 位置由 `index`/`gap`/`size` 在函数内推导,不再作参数传入。
fn button_el(
    s: &BarHud,
    button: Button,
    index: usize,
    size: f32,
    gap: f32,
    t: f32,
    palette: Palette,
) -> El {
    let x = gap + index as f32 * (size + gap);
    let y = gap;
    // 悬停亮底:普通按钮交给 El::hover 的悬停叠加(零视图提交);
    // 月亮的影圆要与亮底同色混合,canvas 画不出悬停叠加,仍读
    // s.hover 画 fill——它一个人保留悬停状态。
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
        .press_on_down()
        .on_press(move |s: &mut BarHud| s.act(button));
    cell = if button == Button::FullWidth {
        cell.on_hover_change(move |s: &mut BarHud, inside, _| {
            s.hover = inside.then_some(button);
        })
        .a11y_value(if s.status.fullwidth {
            "全角"
        } else {
            "半角"
        })
        .child(moon_el(s, size, hovered, palette))
    } else {
        cell.hover(theme::srgb_alpha(palette.hover, 0.66)).child(
            text(toolbar::label(button, s.status))
                .font(TextStyle::new(toolbar::FONT * t, 400.0))
                .color(theme::srgb(palette.text)),
        )
    };
    if hovered {
        cell = cell.fill(theme::srgb_alpha(palette.hover, 0.66));
    }
    cell
}

/// 全半角按钮的月亮:固定的暖黄光源,不跟主题色走——候选窗的胶囊灯管
/// 才跟主题色,月亮是自己的天体。凸起=radial_gradient 的偏心高光
/// (高光在右下,与亮弧同向,边缘沉向左上深黄),月坑=亮区三粒半透明
/// 深琥珀圆。全角=满月,半角=弯月:亮弧贴左下缘,与 🌙 同向。
/// 暗面是「影」:月盘与影圆的交集,canvas 路径一次画成(影圆弧+月盘弧
/// 在两交点闭合)。月相 0→1 时影圆沿左上对角线远去,交集被亮区逐渐
/// 侵蚀到消失——终止线扫过月盘=真月相形变,暗面永远含在月盘内,没有
/// 任何元素飞出按钮。影圆比月盘大 1.5px,静止弯月与定稿的亮弧粗细一致;
/// 月盘弧比盘缘大半像素画,压住盘缘与路径各自抗锯齿对不齐的黄边(实机
/// 翻过车),超出的部分都是按钮底色,看不出来。
/// `hovered` 时影色与按钮底一样混上悬停亮底,缺角不显色差圆。
fn moon_el(s: &BarHud, box_size: f32, hovered: bool, palette: Palette) -> El {
    let d = box_size * 0.62;
    let r = d / 2.0;
    let at = (box_size - d) / 2.0;
    let yellow = theme::srgb(0xfac738);
    // 盘:偏心径向渐变,左下高光到右上深黄边缘,再垫一层平面黄兜底。
    let disc = el()
        .absolute(at, at)
        .w(d)
        .h(d)
        .radius(r)
        .fill(yellow)
        .radial_gradient(
            [0.64, 0.68],
            d,
            [
                (0.0, theme::srgb(0xffe9a0)),
                (0.38, yellow),
                (1.0, theme::srgb(0xc1871e)),
            ],
        )
        .key("moon-disc");
    let mut moon = el().w_full().h_full().child(disc);
    // 月坑:右下亮区三粒,弯月时亮区也能看见两粒。
    for (index, (fx, fy, fd)) in [(0.66, 0.70, 0.17), (0.76, 0.45, 0.12), (0.56, 0.80, 0.09)]
        .into_iter()
        .enumerate()
    {
        moon = moon.child(
            el().absolute((fx - fd / 2.0) * d, (fy - fd / 2.0) * d)
                .w(fd * d)
                .h(fd * d)
                .radius(fd * d / 2.0)
                .fill(theme::srgb_alpha(0x9a6b12, 0.32))
                .key(format!("crater-{index}")),
        );
    }
    // 影:月相值驱动(见函数注释)。
    let p = s.phase.clamp(0.0, 1.0);
    let shadow = if hovered {
        theme::srgb(theme::blend(palette.bg, palette.hover, 0.66))
    } else {
        theme::srgb(palette.bg)
    };
    let dist = (0.622 + 1.7 * p) * r;
    let sr = r + 1.5;
    let rd = r + 0.75;
    let sx = -0.707 * dist;
    let sy = -0.707 * dist;
    let dsq = sx * sx + sy * sy;
    let intersects = dist > 1e-4 && dist < sr + rd && dist > (sr - rd).abs();
    let mut shadow_el = el();
    if intersects {
        let a = (dsq + rd * rd - sr * sr) / (2.0 * dist);
        let h = (rd * rd - a * a).sqrt();
        let ux = sx / dist;
        let uy = sy / dist;
        let mx = ux * a;
        let my = uy * a;
        let px = -uy;
        let py = ux;
        // 画布比月盘大一圈,路径原点随之偏移,放大的月盘弧才有地方画;
        // 交点以月心为原点推得,落进画布要加 o。
        let o = [d / 2.0 + 1.0, d / 2.0 + 1.0];
        let h1 = [o[0] + mx + px * h, o[1] + my + py * h];
        let h2 = [o[0] + mx - px * h, o[1] + my - py * h];
        let s1 = [o[0] + sx, o[1] + sy];
        let shape = shadow_arc_path(o, rd, s1, sr, h1, h2, shadow);
        shadow_el = canvas(shape)
            .absolute(at - 1.0, at - 1.0)
            .w(d + 2.0)
            .h(d + 2.0)
            .key("moon-shadow")
            .a11y_hidden();
    }
    // 光:以月亮为中心的光晕,几何固定;亮度随月相 0→1 增强,差异全在
    // 渐变色标的 alpha 上,与暗面的侵蚀同步。盖在最上层:同距同光,缺角
    // 与周边条底永远一致,黑盘对比无从产生。
    let glow_stops = [
        (0.0, theme::srgb_alpha(0xfac738, 0.10 + 0.18 * p)),
        (0.5, theme::srgb_alpha(0xfac738, 0.05 + 0.09 * p)),
        (1.0, theme::srgb_alpha(0xfac738, 0.0)),
    ];
    let glow = el()
        .absolute(at + d / 2.0 - d * 1.6, at + d / 2.0 - d * 1.6)
        .w(d * 3.2)
        .h(d * 3.2)
        .radial_gradient([0.5, 0.5], d * 1.6, glow_stops)
        .key("moon-glow")
        .a11y_hidden();
    moon.child(shadow_el).child(glow)
}

/// 暗面路径:影圆弧(h1→h2,过近月心点)+ 月盘弧(h2→h1,过影圆方向),
/// 折线近似圆弧(16 段),1x 下足够圆滑。
fn shadow_arc_path(
    o: [f32; 2],
    r: f32,
    s: [f32; 2],
    sr: f32,
    h1: [f32; 2],
    h2: [f32; 2],
    color: Color,
) -> Shape {
    let mut path = rune::Path::new().move_to(h1);
    let a1 = (h1[1] - s[1]).atan2(h1[0] - s[0]);
    let a2 = (h2[1] - s[1]).atan2(h2[0] - s[0]);
    let via1 = (o[1] - s[1]).atan2(o[0] - s[0]);
    let sweep1 = sweep_dir(a1, a2, via1);
    for i in 1..=16 {
        let f = i as f32 / 16.0;
        let t = interp_angle(a1, a2, sweep1, f);
        path = path.line_to([s[0] + sr * t.cos(), s[1] + sr * t.sin()]);
    }
    let b1 = (h2[1] - o[1]).atan2(h2[0] - o[0]);
    let b2 = (h1[1] - o[1]).atan2(h1[0] - o[0]);
    let via2 = (s[1] - o[1]).atan2(s[0] - o[0]);
    let sweep2 = sweep_dir(b1, b2, via2);
    for i in 1..=16 {
        let f = i as f32 / 16.0;
        let t = interp_angle(b1, b2, sweep2, f);
        path = path.line_to([o[0] + r * t.cos(), o[1] + r * t.sin()]);
    }
    Shape::new().fill_path(&path, color)
}

/// 角插值方向:选经过 via 侧的弧。
fn sweep_dir(a: f32, b: f32, via: f32) -> i32 {
    let tau = std::f32::consts::TAU;
    let norm = |x: f32| {
        let mut x = x % tau;
        if x < 0.0 {
            x += tau;
        }
        x
    };
    let (a, b, via) = (norm(a), norm(b), norm(via));
    let cw = norm(b - a);
    let via_cw = norm(via - a);
    if via_cw <= cw { 1 } else { -1 }
}

fn interp_angle(a: f32, b: f32, dir: i32, f: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let mut delta = (b - a) % tau;
    if dir > 0 {
        if delta < 0.0 {
            delta += tau;
        }
    } else if delta > 0.0 {
        delta -= tau;
    }
    a + delta * f
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
/// Windows 起进程要几十毫秒,不占用 UI 帧,挪到助手线程。
fn open_settings() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("open-settings".into())
        .spawn(move || {
            if let Err(e) = std::process::Command::new(dir.join("lueying-settings.exe")).spawn() {
                println!("启动设置失败: {e}");
            }
        });
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
