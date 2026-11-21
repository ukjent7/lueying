//! 候选窗:Rune overlay 承载窗口、玻璃与输入;布局、命中与分页语义
//! 仍来自 `lueying_render::panel`,摆位共用 `ui::place`。
//!
//! 玻璃走自捕获背板:逐帧 BitBlt 窗口区的桌面像素作为背景图,面板
//! `backdrop_blur` 出磨砂玻璃;「动态效果」关掉时回退实底主题。窗口
//! 移动、隐藏与淡出全走 Rune 的 OverlayHandle;管道线程经版本号 +
//! `kick()` 唤醒本线程,鼠标回落事件经 `forward_mouse` 注入同一队列。
//!
//! placement 的 Rect 相对主屏原点(主屏缩放换算);尺寸与内容按光标
//! 所在屏的缩放走。混合 DPI 副屏上的渲染锐度是已知限制,主屏与等比
//! 多屏下位置、尺寸、内容三者一致。

use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use lueying_protocol::{Request, SessionView};
use lueying_render::panel::{self, Item, Layout, Measure, Metrics, PanelSpec, Rect};
use rune::color::srgb_decode;
use rune::{
    App, AppSpec, Color, Cursor, Edge, El, OverlayHandle, OverlayInput, OverlayOptions,
    OverlayPlacement, OverlaySpec, Pixels, Spring, SurfaceFit, TextStyle, Transition, Tx, col, el,
    row, text,
};
use rune_text::TextSystem;

use lueying_server::config::{UiCeramic, UiLayout, UiMaterial};

use crate::imp;
use crate::ui::place;

/// 活背板的重捕节奏(~30fps):背景变动跟随,再高肉眼无感只费电。
const LIVE_TICK: Duration = Duration::from_millis(33);
/// 候选文字最大宽度(逻辑像素):窗口宽度减去页码与翻页箭头的空间。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 组合结束后的淡出时长;期间沿用最后一份内容并让鼠标穿透。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 高透玻璃的模糊强度:40px 条上结构可辨、文字不可读。
const GLASS_BLUR: f32 = 7.0;
/// 磨砂玻璃的模糊强度:更重,接近亚克力。
const FROST_BLUR: f32 = 9.0;
/// 测宽的内部放大倍数:measure 结果按设备像素取整,放大可拿到亚点精度。
const MEASURE_SCALE: f32 = 4.0;
/// CJK 墨迹相对 Rune 行盒的几何,以 Noto Sans SC 17px 实机帧标定,随字号线性。
/// 行盒钉在容器顶时,墨迹顶距盒顶 3px、墨迹高 0.88em。
const INK_TOP: f32 = 3.0 / 17.0;
const INK_HEIGHT: f32 = 15.0 / 17.0;
/// 横排窗口的逻辑高度:恒等于展开棋盘的高度(条贴光标侧,其余透明穿透)。
const WRAP_WINDOW_H: f32 = 210.0;

/// 常驻的 overlay 句柄;别的线程靠它改状态、注入鼠标与唤醒。
static HANDLE: OnceLock<OverlayHandle<Candidate>> = OnceLock::new();

/// 服务端状态的转交点;AppSpec 的 fn 指针从这里取状态。
static RUNE_STATE: OnceLock<Arc<imp::State>> = OnceLock::new();

/// 候选窗的存在状态;截止时刻只在淡出中有效。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Presence {
    /// 无淡出挂起;显示与否由窗口可见性与冻结内容决定。
    #[default]
    Idle,
    /// 原生 alpha 淡出进行中,到期停靠;期间沿用冻结内容并让鼠标穿透。
    FadingOut(Instant),
}

/// 右键菜单状态;悬停槽位只在打开时有效。
#[derive(Clone, Debug, Default, PartialEq)]
enum Menu {
    #[default]
    Closed,
    /// (页内槽位, 悬停项, 行文本)。行文本变了菜单自动失效。
    Open {
        slot: usize,
        text: String,
        hover: Option<usize>,
    },
}

impl Menu {
    /// 打开中的悬停菜单项;关闭时恒为 None。
    fn hover(&self) -> Option<usize> {
        if let Menu::Open { hover, .. } = self {
            *hover
        } else {
            None
        }
    }

    /// 打开中才生效的悬停更新。
    fn set_hover(&mut self, v: Option<usize>) {
        if let Menu::Open { hover, .. } = self {
            *hover = v;
        }
    }
}

/// 候选窗的动画与交互状态;全部归 Rune 的事件线程所有。
#[derive(Default)]
struct Fx {
    /// 淡出状态;期间沿用最后一份内容并让鼠标穿透。
    presence: Presence,
    /// 最后一份组合内容;淡出期间冻结,平时跟最新一帧。
    last: Option<SessionView>,
    /// 本次摆位(窗口+面板,屏幕物理像素);总是整体赋值。
    placement: Placement,
    /// 翻页方向:条内容从这一侧滑入。Rune 的 push 过渡消费。
    slide_edge: Option<Edge>,
    /// 本次组合里候选窗在光标上方还是下方;同一次组合内保持不变。
    above: Option<bool>,
    /// 系统「减少动态效果」;开启后全部动效走终态。
    reduce: bool,
    /// 页内悬停候选。
    hover: Option<usize>,
    /// 悬停的 ⌄ 展开按钮。
    chev: bool,
    /// 鼠标停在棋盘滚动条上;显示加宽态。
    scroll_hover: bool,
    /// 拖动滚动条时鼠标相对滑块顶部的偏移(逻辑点)。
    scroll_drag: Option<f32>,
    /// 右键菜单状态。
    menu: Menu,
    /// 本次组合的起点;外观扩展的时钟从这儿算。
    since: Option<Instant>,
}

impl Fx {
    fn clear(&mut self) {
        *self = Fx::default();
    }

    fn fading(&self) -> bool {
        matches!(self.presence, Presence::FadingOut(_))
    }
}

/// 0..1 的动画进度;已过期钳到 1。
fn progress(total: Duration, end: Instant, now: Instant) -> f32 {
    let remain = end.checked_duration_since(now).unwrap_or_default();
    (1.0 - remain.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

fn ease_out_quint(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(5)
}

/// 一帧的视觉参数:调色板与是否玻璃;控件树的各构件共用。
struct Look<'a> {
    palette: &'a panel::Palette,
    glass: bool,
}

impl Look<'_> {
    /// 悬停/滚动条这类覆盖层一律半透明,玻璃与瓷胎上都成立。
    fn tint(&self, v: u32, alpha: u8) -> Color {
        tint(v, alpha)
    }
}

/// 最近一帧的绘制几何;命中测试与控件树共用,保证所指即所画。
struct Geom {
    spec: PanelSpec,
    metrics: Metrics,
    /// 布局宽高(逻辑点)。
    width: f32,
    height: f32,
}

/// 候选窗 overlay 的状态;Rune 每帧从它重建控件树。
struct Candidate {
    state: Arc<imp::State>,
    fx: Fx,
    /// 文本测量;候选条的自然宽度在显示前就要量好。
    ink: RefCell<TextSystem>,
    /// 玻璃背板:流式像素通道,一次分配,逐帧 `set` 原地换桌面像素。
    backdrop: Option<Pixels>,
    /// 外观扩展的主题色覆盖;None 用系统主题。
    script_accent: Option<u32>,
    /// 上一次见到的视图版本号。
    version: u64,
    /// 候选窗材质;配置重载后随版本号生效。
    material: UiMaterial,
    /// 玻璃磨砂程度 0..=1;高透到磨砂连续可调。
    frost: f32,
    /// 陶瓷胎色;玻璃材质忽略它。
    ceramic: UiCeramic,
    /// 私密字段标记;只影响扩展发布,绘制照常。
    private: bool,
    visible: bool,
    /// 光标屏缩放;placement 与内容共用。
    scale: f32,
    /// 最近一次摆位算出的几何。
    geom: Option<Geom>,
    sent: Option<lueying_ext::proto::candidate::CandidateState>,
    /// 组合序号:每次新组合 +1,编进 matched id——匹配只在同一
    /// 次组合内成立,上一次组合里还活着的元素拿不到它的飞行起点。
    comp: u64,
}

impl Candidate {
    fn with_state(state: Arc<imp::State>) -> Self {
        Self {
            state,
            fx: Fx::default(),
            ink: RefCell::new(TextSystem::new(&[], ("Noto Sans SC", "Noto Sans SC"))),
            backdrop: None,
            script_accent: None,
            version: 0,
            material: UiMaterial::Glass,
            frost: 0.0,
            ceramic: UiCeramic::White,
            private: false,
            visible: false,
            scale: 1.0,
            geom: None,
            sent: None,
            comp: 0,
        }
    }

    /// 命中交互是否可用:激活会话且候选窗在显示。
    fn interactive(&self) -> bool {
        self.state.active.load(Ordering::Relaxed) != 0 && place::composing(&self.state)
    }

    /// 弹性动效开关;配置重载后立即生效。
    fn effects(&self) -> bool {
        self.state
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .effects
    }

    /// 玻璃背板是否启用:动态效果开着且系统没接管外观。
    fn glass_wanted(&self, high_contrast: bool) -> bool {
        !high_contrast && self.effects()
    }

    fn live_glass(&self, high_contrast: bool) -> bool {
        self.material.is_glass()
            && self.visible
            && !self.fx.fading()
            && self.glass_wanted(high_contrast)
    }

    /// 组合结束后的外观时钟;减少动态效果时恒为 1。
    fn clock(&self, now: Instant) -> f32 {
        if self.fx.reduce {
            return 1.0;
        }
        self.fx.since.map_or(1.0, |start| {
            now.saturating_duration_since(start).as_secs_f32()
        })
    }

    /// 淡出分支的进度;静止恒为 1。
    fn anim(&self, now: Instant) -> f32 {
        match self.fx.presence {
            Presence::FadingOut(end) => ease_out_quint(progress(HIDE_FADE, end, now)).max(0.001),
            Presence::Idle => 1.0,
        }
    }

    /// 量一段文字在给定字号下的宽(逻辑点)。
    fn text_width(&self, text: &str, size: f32) -> f32 {
        let mut ink = self.ink.borrow_mut();
        ink.measure(text, TextStyle::new(size, 400.0), MEASURE_SCALE)
            .0
    }
}

impl Measure for Candidate {
    fn width(&self, text: &str, size: f32) -> f32 {
        self.text_width(text, size)
    }
}

/// 0xRRGGBB → Rune 颜色(线性 sRGB)。
fn rgb(v: u32) -> Color {
    let channel = |shift: u32| srgb_decode(((v >> shift) & 0xff) as f32 / 255.0);
    Color {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: 1.0,
    }
}

/// 0xRRGGBB + 线性透明度。
fn rgba(v: u32, a: f32) -> Color {
    let mut color = rgb(v);
    color.a = a;
    color
}

/// 旧画布的 8bit alpha → 0..1。
fn tint(v: u32, alpha: u8) -> Color {
    rgba(v, f32::from(alpha) / 255.0)
}

/// 文本在 h 高容器里垂直居中的 y。Rune 的 center_y 居中的是行盒,
/// CJK 墨迹在行盒里天然偏上;按实机帧标定的墨迹几何补偿。
fn ink_y(h: f32, size: f32) -> f32 {
    h * 0.5 - (INK_TOP + INK_HEIGHT * 0.5) * size
}

/// 液态玻璃的墨色:中性烟熏玻璃配暖白墨,亮暗背板共用一组暖色立住。
/// 选中槽是探针配方的平涂亮黄胶囊,深墨字、深琥珀序号。
fn glass_ink(mut p: panel::Palette) -> panel::Palette {
    p.text = 0xece7df;
    p.text_secondary = 0xe3ddd2;
    p.text_tertiary = 0xd4cec3;
    p.border = 0x4a4238;
    p.hover = 0x3a332a;
    p.accent = 0xd9a92e;
    p.on_accent = 0x2b2110;
    p.selected_bg = 0xfac738;
    p.selected_text = 0x2b2110;
    p.accent_text = 0x7a5608;
    p
}

/// 白瓷的墨色:不透明暖象牙胎配深墨,选中槽同一枚平涂亮黄。
fn ceramic_white_ink(mut p: panel::Palette) -> panel::Palette {
    p.bg = 0xefe9dd;
    p.window_bg = 0xefe9dd;
    p.surface_alt = 0xe6dfd0;
    p.border = 0xd6cdbb;
    p.border_strong = 0xc4bba8;
    p.text = 0x3a352c;
    p.text_secondary = 0x6b6355;
    p.text_tertiary = 0x8a8172;
    p.hover = 0x3a352c;
    p.accent = 0xd9a92e;
    p.on_accent = 0x2b2110;
    p.selected_bg = 0xfac738;
    p.selected_text = 0x2b2110;
    p.accent_text = 0x7a5608;
    p
}

/// 青花瓷的墨色:不透明卵白胎微泛青,藏青墨配钴蓝釉;选中槽是青花蓝。
fn ceramic_blue_ink(mut p: panel::Palette) -> panel::Palette {
    p.bg = 0xf2f6f3;
    p.window_bg = 0xf2f6f3;
    p.surface_alt = 0xe8eeeb;
    p.border = 0xc3d2d8;
    p.border_strong = 0xafc3cc;
    p.text = 0x1d3a5f;
    p.text_secondary = 0x4a6385;
    p.text_tertiary = 0x7d92ac;
    p.hover = 0x1d3a5f;
    p.accent = 0x2e5da0;
    p.on_accent = 0xf5f8f6;
    p.selected_bg = 0x2e5da0;
    p.selected_text = 0xf5f8f6;
    p.accent_text = 0x1d4a85;
    p
}

/// 黑瓷的墨色:不透明暖黑胎配暖白墨,与玻璃同墨不同底。
fn ceramic_black_ink(mut p: panel::Palette) -> panel::Palette {
    p.bg = 0x2e2a24;
    p.window_bg = 0x2e2a24;
    p.surface_alt = 0x262220;
    p.border = 0x4a4238;
    p.border_strong = 0x554c3e;
    p.text = 0xece7df;
    p.text_secondary = 0xe3ddd2;
    p.text_tertiary = 0xd4cec3;
    p.hover = 0x3a332a;
    p.accent = 0xd9a92e;
    p.on_accent = 0x2b2110;
    p.selected_bg = 0xfac738;
    p.selected_text = 0x2b2110;
    p.accent_text = 0x7a5608;
    p
}

/// 当前帧的调色板:玻璃固定暖墨烟熏,实底跟系统主题。
fn palette_of(s: &Candidate, accent: usize, high_contrast: bool) -> panel::Palette {
    let look = match (s.material, s.ceramic) {
        (UiMaterial::Ceramic, UiCeramic::White | UiCeramic::Blue) => {
            lueying_server::theme::Appearance::Light
        }
        _ => lueying_server::theme::Appearance::Dark,
    };
    let colors = match s.script_accent {
        Some(v) if !high_contrast => lueying_server::theme::colors_with_accent(look, v),
        _ => lueying_server::theme::colors_with(look, accent),
    };
    let palette = lueying_server::theme::palette(colors);
    match (s.material, s.ceramic) {
        (UiMaterial::Ceramic, UiCeramic::White) => ceramic_white_ink(palette),
        (UiMaterial::Ceramic, UiCeramic::Blue) => ceramic_blue_ink(palette),
        (UiMaterial::Ceramic, UiCeramic::Black) => ceramic_black_ink(palette),
        _ => glass_ink(palette),
    }
}

/// 取证开关:%TEMP% 里有这个文件就关掉「排除捕获」,截图能看到候选窗;
/// 平时不建文件,录屏/截图里候选窗照旧隐身。
fn capture_exclusion() -> bool {
    !std::env::temp_dir()
        .join("lueying-rune-capture-off")
        .exists()
}

/// 入口:建 Rune 应用与 overlay;事件循环不返回。
pub(crate) fn run(state: Arc<imp::State>) {
    let _ = RUNE_STATE.set(Arc::clone(&state));
    let spec = AppSpec {
        families: ("Noto Sans SC", "Noto Sans SC"),
        ..AppSpec::new(
            "lueying-rune-candidate",
            (480.0, WRAP_WINDOW_H),
            no_window,
            view,
        )
    };
    let app = App::new(spec).windowless().keep_running();
    // windowless 也会构建应用级状态;App::init 允许捕获闭包,把真实状态递进去。
    let app_state = Arc::clone(&state);
    let mut app = app.init(move || Candidate::with_state(app_state));
    // overlay 的窗口标题用独立名,把「lueying-candidate」让给界面线程的
    // 消息桥;TSF 宿主按标题找窗,同名会找到置顶的候选窗本体。
    let mut overlay = OverlaySpec::new("lueying-rune-candidate", (480.0, WRAP_WINDOW_H), view);
    overlay.interactive = true;
    let (x, y) = place::park_point();
    let ((ox, oy), ps) = primary_basis();
    let handle = app.overlay(
        overlay
            .options(OverlayOptions {
                placement: Some(OverlayPlacement::Rect {
                    x: (x - ox) as f32 / ps,
                    y: (y - oy) as f32 / ps,
                    width: 480.0,
                    height: WRAP_WINDOW_H,
                }),
                keys: rune::KeyPolicy::None,
                // 只有标了 hit 的面板区域拦指针;条外透明区原生穿透给宿主。
                input: OverlayInput::Regions,
                exclude_from_capture: capture_exclusion(),
                prewarm: true,
                ..Default::default()
            })
            // 隐藏时归还 GPU 表面:复显时 surface 重建,首帧必是新内容;
            // 否则 DComp 视觉层留着上一帧,会闪上一组句的候选页或展开棋盘。
            .release_gpu_when_hidden()
            .poll(poll),
        Candidate::with_state(state),
    );
    let _ = HANDLE.set(handle);
    app.run();
}

/// App::init 已接管状态构建;这个 fn 指针只是 AppSpec 的必填占位。
fn no_window() -> Candidate {
    Candidate::with_state(Arc::clone(RUNE_STATE.get().expect("候选窗状态未就绪")))
}

/// 管道线程要叫醒候选窗时调用;overlay 起来之前是空操作。
pub(crate) fn kick() {
    if let Some(handle) = HANDLE.get() {
        handle.update(|_: &mut Candidate, _| {});
    }
}

/// 回落钩子转发来的鼠标事件;坐标是屏幕物理像素。
pub(crate) enum FallbackMouse {
    Move(f32, f32),
    Down(f32, f32),
    Up,
    Right(f32, f32),
    Wheel(i16),
}

/// 注入一条回落鼠标事件并唤醒事件循环;overlay 起来之前丢弃。
pub(crate) fn forward_mouse(event: FallbackMouse) {
    if let Some(handle) = HANDLE.get() {
        handle.update(move |s: &mut Candidate, tx| s.on_fallback(event, tx));
    }
}

/// 每帧开头跑一次:补丁、版本、摆位、活背板,并决定下一次唤醒。
fn poll(s: &mut Candidate, _now: f64, tx: &mut Tx) -> bool {
    let appearance = lueying_server::theme::Appearance::of_system();
    let high_contrast = matches!(appearance, lueying_server::theme::Appearance::HighContrast);
    // 外观补丁:晚到的旧补丁直接丢;主题色变了就重绘。
    if let Some((seq, patch)) = s.state.ext.take_patch()
        && seq >= s.state.ext.scene_seq()
    {
        let accent = patch.and_then(|patch| patch.accent);
        if accent != s.script_accent {
            s.script_accent = accent;
        }
    }
    let mut changed = false;
    let version = s.state.version.load(Ordering::Acquire);
    if version != s.version {
        s.version = version;
        s.fx.reduce = reduce_motion();
        let cfg = s.state.config.read().unwrap_or_else(|p| p.into_inner());
        s.material = cfg.ui_material;
        s.frost = cfg.ui_glass_frost.clamp(0.0, 1.0);
        s.ceramic = cfg.ui_ceramic;
        drop(cfg);
        place(s, tx);
        changed = true;
    }
    // 淡出到期停靠;摆位时已排好这一次唤醒。
    if matches!(s.fx.presence, Presence::FadingOut(end) if Instant::now() >= end) {
        s.fx.clear();
        park(s);
        return true;
    }
    if s.live_glass(high_contrast) {
        capture_backdrop(s, high_contrast);
        changed = true;
        tx.wake_after(LIVE_TICK);
    }
    changed
}

/// 一轮摆位:脚本补丁到了就应用;版本变了就摆位与出内容。
fn place(s: &mut Candidate, tx: &mut Tx) {
    let Some(snap) = place::snapshot(&s.state) else {
        // 组合结束:reduce 直接停靠;否则窗口留在原地淡出,到期由轮询停靠。
        if s.fx.reduce || s.fx.last.is_none() {
            s.fx.clear();
            park(s);
            return;
        }
        if !s.fx.fading() {
            s.fx.presence = Presence::FadingOut(Instant::now() + HIDE_FADE);
            s.fx.menu = Menu::Closed;
            s.fx.hover = None;
            s.fx.chev = false;
            if let Some(handle) = HANDLE.get() {
                handle.set_input(OverlayInput::None);
                handle.fade_out(HIDE_FADE);
            }
            tx.wake_after(HIDE_FADE + Duration::from_millis(40));
        }
        publish_state(s, None);
        return;
    };
    let view = snap.view;
    s.private = snap.private;
    let fresh = s.fx.last.is_none() || s.fx.fading();
    if fresh {
        s.comp = s.comp.wrapping_add(1);
        // 新组合撞上进半路的淡出:handle.show 取消排队中的 Hide 请求。
        // 不取消的话,s.visible 仍为真、show() 提前返回,fade_out
        // 照常跑完——这一条组合的候选窗从头到尾不会出现。
        if s.fx.fading()
            && let Some(handle) = HANDLE.get()
        {
            handle.show();
        }
        // 新一次组合重新判断候选窗朝向;候选行数变化时不会上下翻跳。
        s.fx.above = None;
        s.fx.slide_edge = None;
        s.fx.menu = Menu::Closed;
        s.fx.hover = None;
        s.fx.chev = false;
        s.fx.since = Some(Instant::now());
        // 入场过渡只在 animated 提交里播;这一条提交把它打出来。
        tx.animated();
    }
    s.fx.presence = Presence::Idle;
    let placement = caret_rect(s, &view);
    if placement.panel.2 == 0 {
        // 独占全屏或拿不到光标信息:停靠窗口,并清掉冻结内容,
        // 下一次组合按新组合出现,不会接着上一次的状态。
        s.fx.presence = Presence::Idle;
        s.fx.last = None;
        park(s);
        return;
    }
    // 展开/收起:面板矩形在这一提交里变化,交给 Rune 的布局弹簧;
    // 手搓的逐帧改高整套删除。
    let toggled = !fresh
        && s.fx
            .last
            .as_ref()
            .is_some_and(|last| last.expanded != view.expanded);
    if toggled {
        tx.animated();
        // 飞行元素要画出 body 的圆角裁剪之外:格子向"将来棋盘"
        // 的区域飞时被 clip_rounded 裁掉就看不见,等面板长成才浮现。
        tx.unclipped_flights();
        // 槽位重排后旧的悬停坐标已失效;箭头在棋盘里换了格子。
        s.fx.hover = None;
        s.fx.chev = false;
    }
    // 只有固定条翻页才滑动;展开棋盘里挪高亮不横向跳。
    if s.effects()
        && !view.expanded
        && s.fx
            .last
            .as_ref()
            .is_some_and(|last| last.page != view.page)
    {
        let previous = s.fx.last.as_ref().map_or(view.page, |last| last.page);
        s.fx.slide_edge = Some(if view.page > previous {
            Edge::Trailing
        } else {
            Edge::Leading
        });
        tx.animated();
        s.fx.hover = None;
        s.fx.chev = false;
    } else if view.expanded {
        s.fx.slide_edge = None;
    }
    // 同形态、同页内选中迁移:animated 提交让选中指示器弹簧跟过去;
    // 页变走推入、形态变走形变,各自已 animated。
    if s.effects()
        && !fresh
        && s.fx.last.as_ref().is_some_and(|last| {
            last.highlight != view.highlight
                && last.page == view.page
                && last.expanded == view.expanded
        })
    {
        tx.animated();
    }
    s.fx.placement = placement;
    // 回落钩子按面板矩形转发点击;发布给扩展的也是它。
    *s.state
        .candidate_rect
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(placement.panel);
    layout(s, placement.window);
    // 先落内容再出帧;淡出期间用的就是这份冻结内容。
    s.fx.last = Some(view.clone());
    s.geom = Some(geom_of(s, &view));
    submit_scene(s, &view);
    publish_state(s, Some(&view));
    show(s);
    if let Some(handle) = HANDLE.get() {
        handle.set_input(OverlayInput::Regions);
    }
}

/// 一次摆位:overlay 窗口与面板内容各自的屏幕物理矩形。
/// 同属一次摆放,只整体赋值;拆成两个字段的旧写法在移动瞬间会错位。
#[derive(Clone, Copy, Debug, Default)]
struct Placement {
    /// overlay 窗口矩形;背板捕获与内容偏移用它。
    window: (i32, i32, i32, i32),
    /// 面板内容矩形;回落点击转发与扩展发布的矩形用它。
    panel: (i32, i32, i32, i32),
}

/// 光标位置算出的摆位;宽为 0 表示这次不弹。
/// 横排窗口恒为展开棋盘的尺寸,条贴光标侧,其余区域透明且指针穿透;
/// 竖排窗口与面板同矩形。
fn caret_rect(s: &mut Candidate, view: &SessionView) -> Placement {
    let (layout, ui_font, _) = place::ui_options(&s.state);
    let host_pid = (s.state.active.load(Ordering::Relaxed) >> 32) as u32;
    let Some((l, t, r, b)) = view.caret else {
        s.fx.above = None;
        s.state.candidate_above.store(false, Ordering::Relaxed);
        return Placement::default();
    };
    let left = l.min(r);
    // 按光标所在显示器算 DPI;停靠屏的缩放与本屏无关。
    let scale = place::scale_at(left, b);
    s.scale = scale;
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let (mut spec, fit) = place::spec_of(view, layout, &metrics, s);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = place::measure_width(view, layout, &metrics, s);
    if fit != view.page_size {
        let session = s.state.active.load(Ordering::Relaxed);
        let _ = imp::handle(&s.state, Request::SetPageSize { session, size: fit });
    }
    let mut w = panel::panel_width(&spec, &metrics, s) as i32;
    let h = panel::panel_height(&spec, &metrics, s) as i32;
    let work = place::work_area_at(left, b);
    w = w.min((work.right - work.left).max(1));
    let gap = (place::CARET_GAP * scale) as i32;
    // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
    let hidden = place::foreground_fullscreen_at(left, b, host_pid);
    if hidden {
        return Placement::default();
    }
    let horizontal = matches!(layout, UiLayout::Horizontal);
    if horizontal {
        // 横排:窗口恒为棋盘高度;条贴光标侧,展开向上/向下长满整窗。
        let h_win = (WRAP_WINDOW_H * ui_font.factor() * scale) as i32;
        let below = b + gap;
        let above = t - gap - h_win;
        // 同一次组合内优先留在已选的一侧;只有这一侧放不下才翻边。
        // 副屏可以位于主屏上方,`top` 是负数,不能直接 `.max(0)`。
        let fits_below = below + h_win <= work.bottom;
        let fits_above = above >= work.top;
        let prefer_above = match s.fx.above {
            Some(previous) if previous && fits_above => true,
            Some(previous) if !previous && fits_below => false,
            _ => !fits_below && fits_above,
        };
        s.fx.above = Some(prefer_above);
        // 徽标据此躲到候选窗的另一侧,两者不互相遮挡。
        s.state
            .candidate_above
            .store(prefer_above, Ordering::Relaxed);
        let y = (if prefer_above { above } else { below })
            .clamp(work.top, (work.bottom - h_win).max(work.top));
        let x = left.clamp(work.left, (work.right - w).max(work.left));
        let body_y = if prefer_above { y + h_win - h } else { y };
        Placement {
            window: (x, y, w, h_win),
            panel: (x, body_y, w, h),
        }
    } else {
        // 竖排:窗口与面板同矩形,自然高度随行数。
        let below = b + gap;
        let over = t - h - gap;
        let fits_below = below + h <= work.bottom;
        let fits_above = over >= work.top;
        let prefer_above = match s.fx.above {
            Some(previous) if previous && fits_above => true,
            Some(previous) if !previous && fits_below => false,
            _ => !fits_below && fits_above,
        };
        s.fx.above = Some(prefer_above);
        s.state
            .candidate_above
            .store(prefer_above, Ordering::Relaxed);
        let y = (if prefer_above { over } else { below })
            .clamp(work.top, (work.bottom - h).max(work.top));
        let x = left.clamp(work.left, (work.right - w).max(work.left));
        let rect = (x, y, w, h);
        Placement {
            window: rect,
            panel: rect,
        }
    }
}

/// 当前视图的绘制几何;命中测试与控件树共用。
fn geom_of(s: &mut Candidate, view: &SessionView) -> Geom {
    let (layout, ui_font, _) = place::ui_options(&s.state);
    let scale = s.scale;
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let (mut spec, _) = place::spec_of(view, layout, &metrics, s);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = place::measure_width(view, layout, &metrics, s);
    Geom {
        width: panel::panel_width(&spec, &metrics, s),
        height: panel::panel_height(&spec, &metrics, s),
        spec,
        metrics,
    }
}

/// 摆位并把表面尺寸对上;矩形是屏幕物理像素。
fn layout(s: &mut Candidate, rect: (i32, i32, i32, i32)) {
    if rect.2 <= 0 || rect.3 <= 0 {
        return;
    }
    if let Some(handle) = HANDLE.get() {
        handle.set_placement(Some(placement_rect(rect, s.scale)));
    }
}

/// 物理矩形 → placement 的主屏本地逻辑坐标。
fn placement_rect(rect: (i32, i32, i32, i32), scale: f32) -> OverlayPlacement {
    let ((ox, oy), ps) = primary_basis();
    OverlayPlacement::Rect {
        x: (rect.0 - ox) as f32 / ps,
        y: (rect.1 - oy) as f32 / ps,
        width: rect.2 as f32 / scale,
        height: rect.3 as f32 / scale,
    }
}

/// 挪到屏幕外并隐藏;停靠点按当前虚拟桌面算。
fn park(s: &mut Candidate) {
    if s.visible
        && let Some(handle) = HANDLE.get()
    {
        handle.hide();
    }
    s.visible = false;
    let (x, y) = place::park_point();
    if let Some(handle) = HANDLE.get() {
        handle.set_placement(Some(placement_rect(
            (x, y, 480, WRAP_WINDOW_H as i32),
            s.scale,
        )));
    }
    *s.state
        .candidate_rect
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = None;
    s.backdrop = None;
    publish_state(s, None);
}

/// 首帧真正提交后再显示;无焦点显示,不抢宿主焦点。
fn show(s: &mut Candidate) {
    if s.visible {
        return;
    }
    if let Some(handle) = HANDLE.get() {
        handle.show();
    }
    s.visible = true;
}

/// 抓取面板矩形的桌面像素作为玻璃背板;每帧重捕,背景变动跟随。
/// 只捕面板区:它是 backdrop_blur 的唯一底图,面板外的空带不铺
/// 像素,直接透出真桌面,任何捕获残影都到不了那儿。
fn capture_backdrop(s: &mut Candidate, high_contrast: bool) {
    if !s.material.is_glass() || !s.glass_wanted(high_contrast) {
        s.backdrop = None;
        return;
    }
    let (x, y, w, h) = s.fx.placement.panel;
    if w <= 0 || h <= 0 {
        return;
    }
    if let Some((width, height, rgba)) = capture_rect(x, y, w as u32, h as u32) {
        // 流式像素通道:同一张图原地换内容,Rune 唤醒显示它的窗口。
        // 无解码 worker、无 pending 空帧,同尺寸帧复用图集槽位。
        let pixels = s.backdrop.get_or_insert_with(Pixels::new);
        pixels.set(width, height, rgba);
    }
}

/// 把候选窗的当前状态发布给订阅的原生扩展;内容没变就不发。
/// 私密字段的文字在这里就留空,扩展拿不到。
fn publish_state(s: &mut Candidate, view: Option<&SessionView>) {
    let (x, y, w, h) = s.fx.placement.panel;
    let private = s.private;
    let mut state = lueying_ext::proto::candidate::CandidateState {
        visible: s.visible,
        composing: view.is_some(),
        rect: [x, y, w, h],
        private,
        ..Default::default()
    };
    if let Some(view) = view.filter(|_| !private) {
        state.raw.clone_from(&view.raw);
        state.page = view.page;
        state.page_size = view.page_size;
        state.highlight = view.highlight;
        state.candidates.clone_from(&view.candidates);
    }
    if s.sent.as_ref() == Some(&state) {
        return;
    }
    s.state.ext.publish_candidates(state.clone());
    s.sent = Some(state);
}

/// 把场景发给外观扩展;主题色与私密标记都带上。
fn submit_scene(s: &Candidate, view: &SessionView) {
    let now = Instant::now();
    let (_, _, accent) = place::ui_options(&s.state);
    let appearance = lueying_server::theme::Appearance::of_system();
    let high_contrast = matches!(appearance, lueying_server::theme::Appearance::HighContrast);
    let colors = match s.script_accent {
        Some(rgb) if !high_contrast => lueying_server::theme::colors_with_accent(appearance, rgb),
        _ => lueying_server::theme::colors_with(appearance, accent),
    };
    let page_size = view.page_size.max(1);
    let page = panel::page_bounds(view.page, page_size, view.candidates.len());
    let candidates = page.len();
    let selected = if candidates == 0 {
        0
    } else {
        view.highlight
            .saturating_sub(page.start)
            .min(candidates - 1)
    };
    let scene = lueying_ext::proto::SceneIn {
        composing: true,
        page: (view.page + 1) as u32,
        pages: view.candidates.len().div_ceil(page_size).max(1) as u32,
        candidates: candidates as u32,
        selected: selected as u32,
        accent: colors.accent,
        dark: matches!(appearance, lueying_server::theme::Appearance::Dark),
        high_contrast,
        reduce_motion: s.fx.reduce,
        time: s.clock(now),
        anim: s.anim(now),
        private: s.private,
        ..lueying_ext::proto::SceneIn::default()
    };
    s.state.ext.submit_scene(scene);
}

/// 每帧重建候选窗的控件树;几何全部来自 panel.rs,所指即所画。
fn view(s: &Candidate, _cx: &mut rune::Cx<Candidate>) -> El {
    if s.fx.last.is_none() {
        return el();
    }
    let Some(geom) = &s.geom else {
        return el();
    };
    let appearance = lueying_server::theme::Appearance::of_system();
    let high_contrast = matches!(appearance, lueying_server::theme::Appearance::HighContrast);
    // 材质判定与背板是否就位解耦:第一帧没背板也只是少一层模糊,
    // 墨色与选中胶囊从头到尾是同一套,不会闪变材质。
    let glass = s.material.is_glass() && s.glass_wanted(high_contrast);
    // 高透到磨砂的连续插值:压光与模糊同步加重。
    let frost = s.frost.clamp(0.0, 1.0);
    let frost_blur = GLASS_BLUR + (FROST_BLUR - GLASS_BLUR) * frost;
    let frost_fill = rgba(0x262624, 0.40 + 0.22 * frost);
    let frost_top = rgba(0x000000, 0.06 + 0.04 * frost);
    let frost_bottom = rgba(0x000000, 0.22 + 0.08 * frost);
    let (_, _, accent) = place::ui_options(&s.state);
    let palette = palette_of(s, accent, high_contrast);
    let scale = geom.metrics.scale();
    let mut root = el().w_full().h_full();
    // 面板矩形是窗口内的一块:条贴光标侧,展开时铺满。矩形变化交给
    // 布局弹簧,展开/收起的形变由 Rune 原生推进。
    let bx = (s.fx.placement.panel.0 - s.fx.placement.window.0) as f32 / scale;
    let by = (s.fx.placement.panel.1 - s.fx.placement.window.1) as f32 / scale;
    if glass && let Some(pixels) = &s.backdrop {
        // 活背板只铺面板矩形:它只给玻璃的 backdrop_blur 当底图。
        // 面板外的空带不画任何东西,alpha=0 直接透出真桌面——铺整张
        // 捕获图反而会把错位的残影留在候选页即将展开的位置。
        // transition/motion 与 body 同款:展开/收起与入场时跟面板走同一条
        // 轨迹,否则方角补丁会在圆角面板外闪一下。
        root = root.child(
            el().absolute(bx, by)
                .w(s.fx.placement.panel.2 as f32 / scale)
                .h(s.fx.placement.panel.3 as f32 / scale)
                .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
                .motion(Spring::smooth(0.3, 0.0))
                // 方形图像补丁垫在圆角面板下,角上会戳出来——同半径裁圆。
                .clip_rounded(panel::PANEL_RADIUS * scale)
                .pixels_fit(pixels, SurfaceFit::Fill),
        );
    }
    let mut body = col()
        .absolute(bx, by)
        .w(s.fx.placement.panel.2 as f32 / scale)
        .h(s.fx.placement.panel.3 as f32 / scale)
        .hit()
        // 入场:从光标侧滑入并淡入;挂载过渡只在 animated 提交里播,
        // fresh 路径显式打 tx.animated()。顺带盖住复显首帧的透明空窗。
        .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
        .motion(Spring::smooth(0.3, 0.0))
        .radius(panel::PANEL_RADIUS * scale)
        .continuous_corners()
        .clip_rounded(panel::PANEL_RADIUS * scale)
        .on_wheel(|s: &mut Candidate, e, _| {
            s.on_wheel_points(e.dy);
            true
        })
        .on_secondary_press_local(|s: &mut Candidate, p, _| s.on_secondary(p.x, p.y));
    match (s.material, glass) {
        // 玻璃:探针配方的连续插值,滑杆从高透滑到磨砂。
        (UiMaterial::Glass, true) => {
            body = body
                .backdrop_blur(frost_blur)
                .fill(frost_fill)
                .gradient(frost_top, frost_bottom)
                .border(1.0 * scale, rgba(0xffffff, 0.14));
        }
        // 白瓷:不透明暖象牙胎,釉面竖向明暗 + 瓷缘描边;不捕背板。
        (UiMaterial::Ceramic, _) if s.ceramic == UiCeramic::White => {
            body = body
                .gradient(rgb(0xf6f0e6), rgb(0xe4ddd0))
                .border(1.0 * scale, rgb(palette.border));
        }
        // 青花瓷:不透明卵白胎泛青,釉面同构,瓷缘描边带蓝灰。
        (UiMaterial::Ceramic, _) if s.ceramic == UiCeramic::Blue => {
            body = body
                .gradient(rgb(0xf8fbf9), rgb(0xe7eeeb))
                .border(1.0 * scale, rgb(palette.border));
        }
        // 黑瓷:不透明暖黑胎,同构釉面。
        (UiMaterial::Ceramic, _) => {
            body = body
                .gradient(rgb(0x363029), rgb(0x28241d))
                .border(1.0 * scale, rgb(palette.border));
        }
        // 背板/动效不可用:实底主题兜底,绝不露原始桌面。
        _ => {
            body = body
                .fill(rgb(palette.bg))
                .border(1.0 * scale, rgb(palette.border));
        }
    }
    let look = Look {
        palette: &palette,
        glass,
    };
    body = body.child(content(s, geom, &look));
    root = root.child(body);
    if let Menu::Open { slot, text, .. } = &s.fx.menu
        && geom
            .spec
            .items
            .get(*slot)
            .is_some_and(|item| &item.text == text)
    {
        let look = Look {
            palette: &palette,
            glass,
        };
        root = root.child(menu_tree(s, geom, &look, *slot, bx, by));
    }
    root
}

/// 面板内容:固定条或展开棋盘,加右侧控制轨与条尾箭头。
fn content(s: &Candidate, geom: &Geom, look: &Look) -> El {
    let meas: &dyn Measure = s;
    let mut layer = el().w_full().h_full();
    if geom.spec.expand && matches!(geom.spec.layout, Layout::Wrap) {
        // 棋盘:所有候选一样大,文字居中;控制轨收在最右。
        let cells = panel::grid_cells(&geom.spec, &geom.metrics, meas);
        if let Some(rect) = geom
            .spec
            .grid
            .iter()
            .zip(&cells)
            .find(|(c, _)| c.selected)
            .map(|(_, r)| *r)
        {
            layer = layer.child(sel_marker(rect, look, geom.metrics.scale()));
        }
        for (candidate, rect) in geom.spec.grid.iter().zip(&cells) {
            layer = layer.child(cell_el(
                s,
                geom,
                look,
                &candidate.text,
                *rect,
                candidate.selected,
                candidate.index,
            ));
        }
    } else {
        // 固定条:自然宽度候选从左排到右;翻页时整条从页号方向滑入,
        // 旧页向反侧滑出,由 Rune 的过渡原生推进。
        let page = s.fx.last.as_ref().map_or(0, |view| view.page);
        let rects = panel::item_rects(&geom.spec, &geom.metrics, meas);
        let mut strip = el()
            .key(format!("strip-page-{page}"))
            .absolute(0.0, 0.0)
            .w(geom.width)
            .h(geom.height)
            // 只在翻页滑动时播推入;fresh 的 animated 提交里挂载不重演,
            // 否则会与面板入场过渡叠着闪。
            .transition(s.fx.slide_edge.map_or(Transition::none(), Transition::push));
        if let Some(rect) = geom
            .spec
            .items
            .iter()
            .zip(&rects)
            .find(|(i, _)| i.selected)
            .map(|(_, r)| *r)
        {
            strip = strip.child(sel_marker(rect, look, geom.metrics.scale()));
        }
        let page_start = s.fx.last.as_ref().map_or(0, |view| {
            panel::page_bounds(view.page, view.page_size, view.candidates.len()).start
        });
        for (index, (item, rect)) in geom.spec.items.iter().zip(&rects).enumerate() {
            strip = strip.child(item_el(
                s,
                geom,
                look,
                item,
                *rect,
                index,
                page_start + index,
                s.fx.hover == Some(index),
            ));
        }
        layer = layer.child(strip);
    }
    layer = layer.child(control_tree(s, geom, look));
    layer
}

/// 选中指示器:单一共享元素垫在选中槽下,而不是每格各自开关装饰子树。
/// 选中迁移=同一元素的矩形变化,由 `.motion` 弹簧在 animated 提交里跟随;
/// 翻页时它随 strip 整棵重挂在目标位,不飞。
fn sel_marker(rect: Rect, look: &Look, scale: f32) -> El {
    selection_pill(
        el().absolute(rect.left, rect.top)
            .w(rect.width())
            .h(rect.height())
            .radius(rect.height() * 0.5),
        rect,
        look,
        scale,
    )
    // 胶囊不配 matched:它和首候选同一条飞行路线,不透明的胶囊
    // 飞行压在同路径的文字飞行上,展开期间 top1 会整段看不见。
    // 让它随面板形变就地淡入,字的飞行让路优先。
    .transition(Transition::opacity())
    .motion(Spring::smooth(0.2, 0.0))
}

/// 选中槽:整行高的胶囊,上亮下深的釉面 + 贴边镜面与釉根。
fn selection_pill(cell: El, rect: Rect, look: &Look, scale: f32) -> El {
    let (top, bottom) = panel::selection_stops(look.palette.selected_bg, look.glass);
    let (shine, root) = if look.glass {
        (0xfff3cf, 0x6b4300)
    } else {
        (0xffffff, 0x000000)
    };
    let line = |y: f32, rgb_v: u32, alpha: u8| {
        let inset = panel::capsule_inset(rect, rect.top + y) + scale;
        el().absolute(inset, y)
            .w((rect.width() - 2.0 * inset).max(0.0))
            .h((1.0 * scale).max(1.0))
            .fill(tint(rgb_v, alpha))
    };
    cell.gradient(rgb(top), rgb(bottom))
        .child(line(
            1.5 * scale,
            shine,
            if look.glass { 0x66 } else { 0x2e },
        ))
        .child(line(
            rect.height() - 1.5 * scale,
            root,
            if look.glass { 0x4d } else { 0x26 },
        ))
}

/// 固定条的一条候选:序号、文字与注音;选中垫黄釉胶囊。
fn item_el(
    s: &Candidate,
    geom: &Geom,
    look: &Look,
    item: &Item,
    rect: Rect,
    index: usize,
    gidx: usize,
    hovered: bool,
) -> El {
    let scale = geom.metrics.scale();
    let mut cell = col()
        .absolute(rect.left, rect.top)
        .w(rect.width())
        .h(rect.height())
        .radius(rect.height() * 0.5)
        .cursor(Cursor::Pointer)
        // 与棋盘格共享 id:组合序号+全局序号定身份——匹配只在同一
        // 次组合内成立,同文字的不同候选、跨组合的陈年元素都不撞。
        .matched(format!("cand-{}-{gidx}-{}", s.comp, item.text))
        // 宽窄槽间飞行时内容保持原尺寸从前导边裁入,不放大铺满——
        // 否则条上窄格飞进棋盘宽格,字会被按尺寸比放大几帧。
        .matched_crop()
        // matched 飞行需要元素自己的弹簧才动得起来;没有它配对
        // 不上的退出+淡入就退化成"先消失后浮现"。
        .motion(Spring::smooth(0.2, 0.0))
        .on_hover_change(move |s: &mut Candidate, inside, _| {
            s.fx.hover = inside.then_some(index);
        })
        .press_on_down()
        .on_press(move |s: &mut Candidate| s.pick_slot(index));
    if hovered && !item.selected {
        cell = cell.fill(look.tint(look.palette.hover, 0x66));
    }
    let index_ink = if item.selected {
        look.palette.accent_text
    } else {
        look.palette.text_tertiary
    };
    cell = cell
        .child(
            el().absolute(
                panel::ITEM_PAD * scale,
                ink_y(rect.height(), geom.metrics.small),
            )
            .w(20.0 * scale)
            .child(
                text(format!("{}", index + 1))
                    .font(TextStyle::new(geom.metrics.small, 400.0))
                    .color(rgb(index_ink)),
            ),
        )
        .child(
            el().absolute(
                panel::TEXT_X * scale,
                ink_y(rect.height(), geom.metrics.text),
            )
            .child(
                text(ellipsized(s, geom, &item.text))
                    .font(TextStyle::new(geom.metrics.text, 400.0))
                    .color(rgb(if item.selected {
                        look.palette.selected_text
                    } else {
                        look.palette.text
                    })),
            ),
        );
    if !geom.spec.is_strip() && !item.hint.is_empty() {
        let shown = ellipsized(s, geom, &item.text);
        let hint_x = panel::TEXT_X * scale + s.text_width(&shown, geom.metrics.text) + 4.0 * scale;
        cell = cell.child(
            el().absolute(hint_x, ink_y(rect.height(), geom.metrics.small))
                .w(100.0 * scale)
                .child(
                    text(item.hint.clone())
                        .font(TextStyle::new(geom.metrics.small, 400.0))
                        .color(rgb(look.palette.text_tertiary)),
                ),
        );
    }
    cell
}

/// 候选文字按可宽截断;固定条截到条尾,展开与竖排用 max_text_width 兜底。
fn ellipsized(s: &Candidate, geom: &Geom, text: &str) -> String {
    let scale = geom.metrics.scale();
    let budget = if geom.spec.is_strip() {
        (geom.width - panel::PAD_X * scale - panel::TEXT_X * scale - panel::ITEM_PAD * scale)
            .max(1.0)
    } else {
        geom.spec.max_text_width.max(1.0)
    };
    panel::ellipsize(text, budget, geom.metrics.text, s)
}

/// 右侧控制轨:竖槽 + 滚动条 + 条尾箭头;拖动与悬停都挂在这条轨上。
fn control_tree(s: &Candidate, geom: &Geom, look: &Look) -> El {
    let meas: &dyn Measure = s;
    let scale = geom.metrics.scale();
    let mut layer = el().w_full().h_full();
    if let Some(rail) = panel::rail_rect(&geom.spec, &geom.metrics, meas, geom.height) {
        layer = layer.child(
            el().absolute(rail.left, rail.top)
                .w(rail.width())
                .h(rail.height())
                .radius(rail.width() * 0.5)
                .fill(tint(0x000000, 0x34))
                .border(1.0 * scale, tint(0xffffff, 0x14))
                .on_hover_change(|s: &mut Candidate, inside, _| {
                    s.fx.scroll_hover = inside;
                })
                .on_drag(|s: &mut Candidate, drag, _| match drag.phase {
                    rune::Phase::Down => {
                        let [x, y] = drag.window_point();
                        let (px, py) = s.window_to_panel(x, y);
                        s.scroll_grab_at(px, py);
                    }
                    rune::Phase::Move => {
                        let [_, y] = drag.window_point();
                        let (_, py) = s.window_to_panel(0.0, y);
                        s.scroll_drag_to(py);
                    }
                    rune::Phase::Up => s.fx.scroll_drag = None,
                }),
        );
    }
    if let Some(scroll) = geom.spec.scroll
        && let Some((track, thumb)) = panel::scrollbar_rects(
            scroll,
            geom.width,
            geom.height,
            &geom.metrics,
            s.fx.scroll_hover,
        )
    {
        let (track_a, thumb_a) = if s.fx.scroll_hover {
            (0x55, 0xdd)
        } else {
            (0x40, 0xbb)
        };
        layer = layer
            .child(
                el().absolute(track.left, track.top)
                    .w(track.width())
                    .h(track.height())
                    .radius(track.width() * 0.5)
                    .fill(look.tint(look.palette.text_tertiary, track_a)),
            )
            .child(
                el().absolute(thumb.left, thumb.top)
                    .w(thumb.width())
                    .h(thumb.height())
                    .radius(thumb.width() * 0.5)
                    .fill(look.tint(look.palette.text_tertiary, thumb_a)),
            );
    }
    if let Some(rect) = panel::chevron_rect(&geom.spec, &geom.metrics, meas) {
        let expanded = geom.spec.expand && matches!(geom.spec.layout, Layout::Wrap);
        let scrollable = geom
            .spec
            .scroll
            .is_some_and(|scroll| scroll.cap > 0 && scroll.len > scroll.cap);
        let mut chev = el()
            .absolute(rect.left, rect.top)
            .w(rect.width())
            .h(rect.height())
            .radius(rect.height() * 0.5)
            .cursor(Cursor::Pointer)
            .on_hover_change(|s: &mut Candidate, inside, _| s.fx.chev = inside)
            .press_on_down()
            .on_press(|s: &mut Candidate| s.expand_toggle());
        if expanded && scrollable {
            if s.fx.chev {
                chev = chev.fill(look.tint(look.palette.border, 0x66));
            }
        } else if expanded {
            let alpha = if s.fx.chev { 0x66 } else { 0x38 };
            chev = chev.fill(look.tint(look.palette.border, alpha));
        } else if s.fx.chev {
            chev = chev.fill(look.tint(look.palette.border, 0x4d));
        }
        let glyph = if expanded && scrollable {
            let size = 14.0 * scale;
            Rect::from_xywh(
                rect.center_x() - size * 0.5,
                rect.center_y() - size * 0.5,
                size,
                size,
            )
        } else {
            rect
        };
        let alpha = if expanded && scrollable && !s.fx.chev {
            0x8c
        } else {
            0xff
        };
        let (color, width) = (look.tint(look.palette.text_secondary, alpha), 1.6 * scale);
        let turn = if geom.spec.expand {
            std::f32::consts::PI
        } else {
            0.0
        };
        // 点位是面板坐标;线段是容器(圆钮)的子元素,要换算进容器内。
        let [a, b, c] = panel::chevron_points_turn(glyph, turn, scale);
        let (a, b, c) = (
            (a.x - rect.left, a.y - rect.top),
            (b.x - rect.left, b.y - rect.top),
            (c.x - rect.left, c.y - rect.top),
        );
        chev = chev
            .child(segment_el(a, b, width, color))
            .child(segment_el(b, c, width, color));
        layer = layer.child(chev);
    }
    layer
}

/// 两点之间的一段圆帽线;Rune 没有裸线原语,用细圆角矩形旋转到位。
fn segment_el(a: (f32, f32), b: (f32, f32), width: f32, color: Color) -> El {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let cx = (a.0 + b.0) * 0.5;
    let cy = (a.1 + b.1) * 0.5;
    el().absolute(cx - len * 0.5, cy - width * 0.5)
        .w(len)
        .h(width)
        .radius(width * 0.5)
        .fill(color)
        .rotate(dy.atan2(dx))
}

/// 右键菜单:固定形状,外层 10px、行内缩 4px;玻璃上也用实底。
fn menu_tree(s: &Candidate, geom: &Geom, look: &Look, slot: usize, bx: f32, by: f32) -> El {
    let meas: &dyn Measure = s;
    let (mx, my) = menu_origin(slot, &geom.spec, &geom.metrics, meas);
    let scale = geom.metrics.scale();
    let w = 116.0 * scale;
    let h = panel::menu_height(&geom.metrics);
    let mut card = col()
        .absolute(mx + bx, my + by)
        .hit()
        .w(w)
        .h(h)
        .radius(10.0 * scale)
        .fill(rgb(look.palette.bg))
        .border(1.0 * scale, rgb(look.palette.border));
    for (index, label) in panel::MENU_ITEMS.iter().enumerate() {
        let y = (4.0 + index as f32 * 28.0) * scale;
        let hovered = s.fx.menu.hover() == Some(index);
        let mut row = el()
            .absolute(4.0 * scale, y)
            .w((w - 8.0 * scale).max(0.0))
            .h(26.0 * scale)
            .radius(panel::concentric_radius(10.0 * scale, 4.0 * scale, 0.0))
            .px(4.0 * scale)
            .cursor(Cursor::Pointer)
            .on_hover_change(move |s: &mut Candidate, inside, _| {
                s.fx.menu.set_hover(inside.then_some(index));
            })
            .press_on_down()
            .on_press(move |s: &mut Candidate| s.menu_click(slot, index));
        if hovered {
            row = row.fill(rgb(look.palette.hover));
        }
        card = card.child(
            row.child(
                el().absolute(0.0, ink_y(26.0 * scale, 12.0 * scale)).child(
                    text((*label).to_string())
                        .font(TextStyle::new(12.0 * scale, 400.0))
                        .color(rgb(if hovered {
                            look.palette.text
                        } else {
                            look.palette.text_secondary
                        })),
                ),
            ),
        );
    }
    card
}

/// 菜单锚点:压着指定候选行,画在面板内部;锚不下就贴行顶。
fn menu_origin(
    slot: usize,
    spec: &PanelSpec,
    metrics: &Metrics,
    measure: &dyn Measure,
) -> (f32, f32) {
    let scale = metrics.scale();
    let panel_h = panel::panel_height(spec, metrics, measure);
    let menu_h = panel::menu_height(metrics);
    let rects = panel::item_rects(spec, metrics, measure);
    // 菜单只画在面板内部:上界是首行行顶,锚不下就压着行画。
    let floor = rects.first().map_or(0.0, |first| first.top);
    let Some(rect) = rects.get(slot).copied() else {
        return (8.0 * scale, floor);
    };
    let top = if rect.bottom + menu_h <= panel_h {
        rect.bottom
    } else {
        (rect.top - menu_h).max(floor)
    };
    (8.0 * scale, top)
}

impl Candidate {
    /// 发一条服务端请求;回落宿主没有 TSF 的 Sync,点完当场注入。
    fn request(&self, req: Request) {
        let _ = imp::handle(&self.state, req);
        imp::fallback::flush(&self.state);
    }

    /// 窗口物理像素 → 面板逻辑点;拖动滚动条等窗口坐标回调用。
    fn window_to_panel(&self, x: f32, y: f32) -> (f32, f32) {
        let scale = self.geom.as_ref().map_or(1.0, |geom| geom.metrics.scale());
        (
            (x - self.fx.placement.panel.0 as f32) / scale,
            (y - self.fx.placement.panel.1 as f32) / scale,
        )
    }

    fn pick_slot(&mut self, slot: usize) {
        if !self.interactive() {
            return;
        }
        let session = self.state.active.load(Ordering::Relaxed);
        self.request(Request::Pick { session, slot });
    }

    fn pick_index(&mut self, index: usize) {
        if !self.interactive() {
            return;
        }
        let session = self.state.active.load(Ordering::Relaxed);
        self.request(Request::PickIndex { session, index });
    }

    fn expand_toggle(&mut self) {
        if !self.interactive() {
            return;
        }
        let session = self.state.active.load(Ordering::Relaxed);
        self.request(Request::Expand { session });
    }

    fn menu_click(&mut self, slot: usize, item: usize) {
        if !self.interactive() {
            return;
        }
        let session = self.state.active.load(Ordering::Relaxed);
        let req = if item == 0 {
            Request::Pin { session, slot }
        } else {
            Request::Demote { session, slot }
        };
        self.request(req);
        self.fx.menu = Menu::Closed;
    }

    fn scroll_to(&mut self, offset: usize) {
        let session = self.state.active.load(Ordering::Relaxed);
        let _ = imp::handle(&self.state, Request::ScrollTo { session, offset });
    }

    /// 滚轮一档:固定条上滚轮展开棋盘,棋盘里滚轮翻整页。
    fn wheel_step(&mut self, step: i32) {
        if !self.interactive() {
            return;
        }
        let session = self.state.active.load(Ordering::Relaxed);
        let Some(published) = self.state.active_published() else {
            return;
        };
        let (expanded, len) = (published.view.expanded, published.view.candidates.len());
        if len > 1 {
            if expanded {
                self.request(Request::Page {
                    session,
                    delta: step,
                });
            } else {
                self.request(Request::Expand { session });
            }
        }
    }

    /// Rune 滚轮事件:dy 正是向下滚,翻到下一页。
    fn on_wheel_points(&mut self, dy: f32) {
        if dy == 0.0 {
            return;
        }
        self.wheel_step(if dy > 0.0 { 1 } else { -1 });
    }

    /// 右键:固定条塞不下菜单,先切展开态,菜单一帧后锚在整词行上。
    fn on_secondary(&mut self, x: f32, y: f32) {
        if !self.interactive() {
            return;
        }
        let Some(geom) = &self.geom else {
            return;
        };
        let meas: &dyn Measure = self;
        let panel::Hit::Item(slot) = panel::hit_test(&geom.spec, &geom.metrics, meas, x, y) else {
            return;
        };
        let Some(item) = geom.spec.items.get(slot) else {
            return;
        };
        let strip = matches!(geom.spec.layout, panel::Layout::Wrap) && !geom.spec.expand;
        if strip {
            let session = self.state.active.load(Ordering::Relaxed);
            let _ = imp::handle(&self.state, Request::Expand { session });
        }
        self.fx.menu = Menu::Open {
            slot,
            text: item.text.clone(),
            hover: None,
        };
    }

    /// 回落钩子注入的鼠标事件;屏幕物理像素转客户区后走同一路径。
    fn on_fallback(&mut self, event: FallbackMouse, _tx: &mut Tx) {
        let (ox, oy) = (
            self.fx.placement.panel.0 as f32,
            self.fx.placement.panel.1 as f32,
        );
        match event {
            FallbackMouse::Move(x, y) => self.fallback_move(x - ox, y - oy),
            FallbackMouse::Down(x, y) => self.fallback_down(x - ox, y - oy),
            FallbackMouse::Up => self.fx.scroll_drag = None,
            FallbackMouse::Right(x, y) => self.on_secondary(x - ox, y - oy),
            FallbackMouse::Wheel(delta) => {
                self.wheel_step(if delta > 0 { -1 } else { 1 });
            }
        }
    }

    fn fallback_move(&mut self, x: f32, y: f32) {
        if self.fx.fading() {
            return;
        }
        // 拖动滚动条:鼠标可能移出窗口,坐标按回落钩子继续喂。
        if self.fx.scroll_drag.is_some() {
            self.scroll_drag_to(y);
            return;
        }
        let (item, chev, scroll_hover, menu_hover) = self.hover_hit(x, y);
        if item != self.fx.hover
            || chev != self.fx.chev
            || menu_hover != self.fx.menu.hover()
            || scroll_hover != self.fx.scroll_hover
        {
            self.fx.hover = item;
            self.fx.chev = chev;
            self.fx.menu.set_hover(menu_hover);
            self.fx.scroll_hover = scroll_hover;
        }
    }

    fn fallback_down(&mut self, x: f32, y: f32) {
        if !self.interactive() {
            return;
        }
        let raw = self.raw_hit(x, y);
        // 滚动条优先:滑块直接拖,轨道空白处跳转后继续拖;⌄ 区域除外。
        if !matches!(raw, panel::Hit::Expand) && self.scroll_grab_at(x, y) {
            return;
        }
        // 菜单项优先于候选行。
        if let Menu::Open { slot, .. } = &self.fx.menu
            && let Some(geom) = &self.geom
        {
            let meas: &dyn Measure = self;
            let (mx, my) = menu_origin(*slot, &geom.spec, &geom.metrics, meas);
            if let Some(item) = panel::menu_hit(mx, my, &geom.metrics, x, y) {
                self.menu_click(*slot, item);
                return;
            }
        }
        match raw {
            panel::Hit::Item(slot) => self.pick_slot(slot),
            panel::Hit::GridItem(index) => self.pick_index(index),
            panel::Hit::Expand => self.expand_toggle(),
            panel::Hit::Nothing => {}
        }
    }

    /// 无菜单/滚动条压制时的原始命中。
    fn raw_hit(&self, x: f32, y: f32) -> panel::Hit {
        let Some(geom) = &self.geom else {
            return panel::Hit::Nothing;
        };
        panel::hit_test(&geom.spec, &geom.metrics, self, x, y)
    }

    /// (候选槽位, ⌄, 滚动条悬停, 菜单项) 四路命中;菜单与滚动条压住的行不参与悬停。
    fn hover_hit(&self, x: f32, y: f32) -> (Option<usize>, bool, bool, Option<usize>) {
        let Some(geom) = &self.geom else {
            return (None, false, false, None);
        };
        let meas: &dyn Measure = self;
        let raw = panel::hit_test(&geom.spec, &geom.metrics, meas, x, y);
        let expand = matches!(raw, panel::Hit::Expand);
        // 滚动条的隐形命中区压过最右一列候选的右缘;⌄ 优先。
        let scroll_hover = !expand
            && geom.spec.scroll.is_some_and(|scroll| {
                panel::scrollbar_hit(scroll, geom.width, geom.height, &geom.metrics, x, y).is_some()
            });
        let menu_hover = if let Menu::Open { slot, .. } = &self.fx.menu {
            let (mx, my) = menu_origin(*slot, &geom.spec, &geom.metrics, meas);
            panel::menu_hit(mx, my, &geom.metrics, x, y)
        } else {
            None
        };
        let hit = if menu_hover.is_some() || scroll_hover {
            panel::Hit::Nothing
        } else {
            raw
        };
        let item = if let panel::Hit::Item(index) = hit {
            Some(index)
        } else {
            None
        };
        let chev = matches!(hit, panel::Hit::Expand);
        (item, chev, scroll_hover, menu_hover)
    }

    /// 按下滚动条:滑块直接拖,轨道空白处跳转后继续拖。
    fn scroll_grab_at(&mut self, x: f32, y: f32) -> bool {
        let plan = self.geom.as_ref().and_then(|geom| {
            let scroll = geom.spec.scroll?;
            let hit = panel::scrollbar_hit(scroll, geom.width, geom.height, &geom.metrics, x, y)?;
            let (_, thumb) =
                panel::scrollbar_rects(scroll, geom.width, geom.height, &geom.metrics, true)?;
            Some(match hit {
                panel::ScrollHit::Thumb => ((y - thumb.top).clamp(0.0, thumb.height()), None),
                panel::ScrollHit::Track => {
                    let offset = panel::scrollbar_offset(
                        scroll,
                        geom.width,
                        geom.height,
                        &geom.metrics,
                        y,
                        Some(thumb.height() * 0.5),
                    );
                    (thumb.height() * 0.5, Some((offset, scroll.offset)))
                }
            })
        });
        let Some((grab, jump)) = plan else {
            return false;
        };
        if let Some((offset, current)) = jump
            && offset != current
        {
            self.scroll_to(offset);
        }
        self.fx.scroll_drag = Some(grab);
        self.fx.scroll_hover = true;
        true
    }

    /// 拖动中把鼠标 y 换算成行对齐的 offset;变了就发请求。
    fn scroll_drag_to(&mut self, y: f32) {
        let plan = self.geom.as_ref().and_then(|geom| {
            let scroll = geom.spec.scroll?;
            let grab = self.fx.scroll_drag?;
            Some((
                panel::scrollbar_offset(
                    scroll,
                    geom.width,
                    geom.height,
                    &geom.metrics,
                    y,
                    Some(grab),
                ),
                scroll.offset,
            ))
        });
        if let Some((offset, current)) = plan
            && offset != current
        {
            self.scroll_to(offset);
        }
    }
}

/// 抓取一块屏幕区域的桌面像素(RGBA,straight alpha)。
#[cfg(windows)]
fn capture_rect(x: i32, y: i32, width: u32, height: u32) -> Option<(u32, u32, Vec<u8>)> {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
    };

    if width == 0 || height == 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory = CreateCompatibleDC(Some(screen));
        if memory.is_invalid() {
            ReleaseDC(None, screen);
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = null_mut();
        let bitmap = match CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        {
            Ok(bitmap) => bitmap,
            Err(_) => {
                let _ = DeleteDC(memory);
                ReleaseDC(None, screen);
                return None;
            }
        };
        let old = SelectObject(memory, bitmap.into());
        let copied = BitBlt(
            memory,
            0,
            0,
            width as i32,
            height as i32,
            Some(screen),
            x,
            y,
            SRCCOPY,
        )
        .is_ok();
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        if copied && !bits.is_null() {
            std::ptr::copy_nonoverlapping(bits.cast::<u8>(), pixels.as_mut_ptr(), pixels.len());
        }
        SelectObject(memory, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        if !copied {
            return None;
        }
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Some((width, height, pixels))
    }
}

#[cfg(not(windows))]
fn capture_rect(_x: i32, _y: i32, _width: u32, _height: u32) -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// 主屏原点与缩放;placement 的 Rect 相对主屏原点。
#[cfg(windows)]
fn primary_basis() -> ((i32, i32), f32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).unwrap_or_default(),
            ..Default::default()
        };
        let origin = if GetMonitorInfoW(monitor, &mut info).as_bool() {
            (info.rcMonitor.left, info.rcMonitor.top)
        } else {
            (0, 0)
        };
        let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
        let scale = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok()
            && dpi_x > 0
        {
            dpi_x as f32 / 96.0
        } else {
            1.0
        };
        (origin, scale)
    }
}

#[cfg(not(windows))]
fn primary_basis() -> ((i32, i32), f32) {
    ((0, 0), 1.0)
}

/// 系统「减少动态效果」;开启后动效全部走终态。查询失败按可用处理。
#[cfg(windows)]
pub(crate) fn reduce_motion() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    use windows::core::BOOL;

    let mut enabled = BOOL::default();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut enabled as *mut BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    ok && !enabled.as_bool()
}

#[cfg(not(windows))]
fn reduce_motion() -> bool {
    false
}

/// 棋盘的一格:选中时垫同款黄釉胶囊,文字在格内居中。
fn cell_el(
    s: &Candidate,
    geom: &Geom,
    look: &Look,
    candidate: &str,
    rect: Rect,
    selected: bool,
    index: usize,
) -> El {
    let scale = geom.metrics.scale();
    let meas: &dyn Measure = s;
    let budget = (rect.width() - 16.0 * scale).max(1.0);
    let shown = panel::ellipsize(candidate, budget, geom.metrics.text, meas);
    let cell = col()
        .absolute(rect.left, rect.top)
        .w(rect.width())
        .h(rect.height())
        .radius(rect.height() * 0.5)
        .px(8.0 * scale)
        .cursor(Cursor::Pointer)
        // 与固定条候选共享 id:组合序号+全局序号定身份——匹配只在同一
        // 次组合内成立,同文字的不同候选、跨组合的陈年元素都不撞。
        .matched(format!("cand-{}-{index}-{candidate}", s.comp))
        // 宽窄槽间飞行时内容保持原尺寸从前导边裁入,不放大铺满——
        // 否则条上窄格飞进棋盘宽格,字会被按尺寸比放大几帧。
        .matched_crop()
        // matched 飞行需要元素自己的弹簧才动得起来;没有它配对
        // 不上的退出+淡入就退化成"先消失后浮现"。
        .motion(Spring::smooth(0.2, 0.0))
        .press_on_down()
        .on_press(move |s: &mut Candidate| s.pick_index(index));
    cell.child(
        row()
            .mt(ink_y(rect.height(), geom.metrics.text))
            .w_full()
            .child(el().grow(1.0))
            .child(
                text(shown)
                    .font(TextStyle::new(geom.metrics.text, 400.0))
                    .color(rgb(if selected {
                        look.palette.selected_text
                    } else {
                        look.palette.text
                    })),
            )
            .child(el().grow(1.0)),
    )
}
