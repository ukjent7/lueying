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
    App, AppSpec, Color, Cursor, El, OverlayHandle, OverlayInput, OverlayOptions, OverlayPlacement,
    OverlaySpec, Pixels, SurfaceFit, TextStyle, Tx, col, el, row, text,
};
use rune_text::TextSystem;

use lueying_server::config::{UiCeramic, UiMaterial};

use crate::imp;
use crate::ui::place;

/// 淡出/滑动期间的重绘节奏。
const ANIM_TICK: Duration = Duration::from_millis(5);
/// 展开形变按 ~60fps 推进:窗口逐帧改高,足够顺滑也省电。
const EXPAND_TICK: Duration = Duration::from_millis(16);
/// 活背板的重捕节奏(~30fps):背景变动跟随,再高肉眼无感只费电。
const LIVE_TICK: Duration = Duration::from_millis(33);
/// 候选文字最大宽度(逻辑像素):窗口宽度减去页码与翻页箭头的空间。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 组合结束后的淡出时长;期间沿用最后一份内容并让鼠标穿透。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 翻页时候选内容横向滑入,90ms 内归位。
const PAGE_SLIDE: Duration = Duration::from_millis(90);
/// 停顿重排换了首选时,候选列表轻微变暗再亮起的时长。
const PROMOTE_FLASH: Duration = Duration::from_millis(160);
/// 展开/收起的形变时长;弹簧在 300ms 内收敛,与 Apple 的 smooth 预设同量级。
const EXPAND_MS: Duration = Duration::from_millis(300);
/// 高透玻璃的模糊强度:40px 条上结构可辨、文字不可读。
const GLASS_BLUR: f32 = 7.0;
/// 磨砂玻璃的模糊强度:更重,接近亚克力。
const FROST_BLUR: f32 = 9.0;
/// 测宽的内部放大倍数:measure 结果按设备像素取整,放大可拿到亚点精度。
const MEASURE_SCALE: f32 = 4.0;

/// 常驻的 overlay 句柄;别的线程靠它改状态、注入鼠标与唤醒。
static HANDLE: OnceLock<OverlayHandle<Candidate>> = OnceLock::new();

/// 服务端状态的转交点;AppSpec 的 fn 指针从这里取状态。
static RUNE_STATE: OnceLock<Arc<imp::State>> = OnceLock::new();

/// 展开/收起的形变:同一块玻璃的窗口矩形从旧态长到新态。
#[derive(Clone, Copy)]
struct ExpandFx {
    from: (i32, i32, i32, i32),
    to: (i32, i32, i32, i32),
    start: Instant,
    end: Instant,
    /// 候选窗在光标上方:底边贴住光标侧不动,向上生长。
    up: bool,
}

impl ExpandFx {
    /// 弹簧进度;「减少动态效果」由调用方直接跳过,不走到这里。
    fn progress(&self, now: Instant) -> f32 {
        let total = self.end.saturating_duration_since(self.start).as_secs_f32();
        if total <= 0.0 {
            return 1.0;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / total;
        spring_out(t.clamp(0.0, 1.0))
    }

    /// 当前窗口矩形;高度按进度插值,贴住光标的那条边保持不动。
    fn rect(&self, now: Instant) -> (i32, i32, i32, i32) {
        let p = self.progress(now);
        let lerp = |a: i32, b: i32| (a as f32 + (b - a) as f32 * p).round() as i32;
        let h = lerp(self.from.3, self.to.3).max(1);
        let x = lerp(self.from.0, self.to.0);
        let w = lerp(self.from.2, self.to.2).max(1);
        let y = if self.up {
            self.to.1 + self.to.3 - h
        } else {
            self.to.1
        };
        (x, y, w, h)
    }
}

/// 临界阻尼弹簧:起步快、收尾稳,没有可见回弹。归一化到端点 1。
fn spring_out(t: f32) -> f32 {
    const K: f32 = 6.0;
    let curve = |t: f32| {
        let kt = K * t;
        1.0 - (1.0 + kt) * (-kt).exp()
    };
    curve(t) / curve(1.0)
}

/// 候选窗的动画与交互状态;全部归 Rune 的事件线程所有。
#[derive(Default)]
struct Fx {
    /// 淡出截止时刻;期间沿用最后一份内容并让鼠标穿透。
    hide: Option<Instant>,
    /// 最后一份组合内容;淡出期间冻结,平时跟最新一帧。
    last: Option<SessionView>,
    /// 正在进行的翻页滑动:方向与截止时刻。
    slide: Option<(f32, Instant)>,
    /// 上一帧的页码,用来判断翻页方向。
    page: Option<usize>,
    /// 换首选提示的截止时刻。
    flash: Option<Instant>,
    /// 展开/收起的窗口形变;静止时为 None。
    expand: Option<ExpandFx>,
    /// 上一次看到的换首选次数;变化即触发一次提示。
    promotions: u32,
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
    /// 打开的右键菜单:(页内槽位, 占位, 行文本)。行文本变了菜单自动失效。
    menu: Option<(usize, f32, String)>,
    /// 悬停的菜单项。
    menu_hover: Option<usize>,
    /// 本次组合的起点;外观扩展的时钟从这儿算。
    since: Option<Instant>,
    /// 静止位置(屏幕坐标);发布给扩展的矩形用它。
    rest: (i32, i32, i32, i32),
}

impl Fx {
    fn clear(&mut self) {
        *self = Fx::default();
    }

    fn fading(&self) -> bool {
        self.hide.is_some()
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
    /// 窗口逻辑尺寸(物理/缩放),控件树按它铺满。
    logical: (f32, f32),
    /// 最近一次摆位算出的几何。
    geom: Option<Geom>,
    sent: Option<lueying_ext::proto::candidate::CandidateState>,
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
            logical: (480.0, 40.0),
            geom: None,
            sent: None,
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

    /// 动画进行中按动画帧推进;活背板可见时按 LIVE_TICK 推帧。
    fn busy_animating(&self) -> bool {
        self.fx.hide.is_some()
            || self.fx.slide.is_some()
            || self.fx.flash.is_some()
            || self.fx.expand.is_some()
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
        self.fx.hide.map_or(1.0, |end| {
            ease_out_quint(progress(HIDE_FADE, end, now)).max(0.001)
        })
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
        (UiMaterial::Ceramic, UiCeramic::White) => lueying_server::theme::Appearance::Light,
        _ => lueying_server::theme::Appearance::Dark,
    };
    let colors = match s.script_accent {
        Some(v) if !high_contrast => lueying_server::theme::colors_with_accent(look, v),
        _ => lueying_server::theme::colors_with(look, accent),
    };
    let palette = lueying_server::theme::palette(colors);
    match (s.material, s.ceramic) {
        (UiMaterial::Ceramic, UiCeramic::White) => ceramic_white_ink(palette),
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
        ..AppSpec::new("lueying-rune-candidate", (480.0, 40.0), no_window, view)
    };
    let app = App::new(spec).windowless().keep_running();
    // windowless 也会构建应用级状态;App::init 允许捕获闭包,把真实状态递进去。
    let app_state = Arc::clone(&state);
    let mut app = app.init(move || Candidate::with_state(app_state));
    // overlay 的窗口标题用独立名,把「lueying-candidate」让给界面线程的
    // 消息桥;TSF 宿主按标题找窗,同名会找到置顶的候选窗本体。
    let mut overlay = OverlaySpec::new("lueying-rune-candidate", (480.0, 40.0), view);
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
                    height: 40.0,
                }),
                keys: rune::KeyPolicy::None,
                input: OverlayInput::All,
                exclude_from_capture: capture_exclusion(),
                prewarm: true,
                ..Default::default()
            })
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

/// 每帧开头跑一次:补丁、版本、动画、活背板,并决定下一次唤醒。
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
        place(s);
        changed = true;
    }
    if s.busy_animating() {
        animate(s);
        changed = true;
    } else if s.live_glass(high_contrast) {
        capture_backdrop(s, high_contrast);
        changed = true;
    }
    if s.fx.expand.is_some() {
        tx.wake_after(EXPAND_TICK);
    } else if s.busy_animating() {
        tx.wake_after(ANIM_TICK);
    } else if s.live_glass(high_contrast) {
        tx.wake_after(LIVE_TICK);
    }
    changed
}

/// 一轮摆位:脚本补丁到了就应用;版本变了就摆位与出内容。
fn place(s: &mut Candidate) {
    let Some(snap) = place::snapshot(&s.state) else {
        // 组合结束:reduce 直接停靠;否则窗口留在原地淡出,到期由动画帧停靠。
        if s.fx.reduce || s.fx.last.is_none() {
            s.fx.clear();
            park(s);
            return;
        }
        if s.fx.hide.is_none() {
            s.fx.hide = Some(Instant::now() + HIDE_FADE);
            s.fx.slide = None;
            s.fx.expand = None;
            s.fx.menu = None;
            s.fx.menu_hover = None;
            s.fx.hover = None;
            s.fx.chev = false;
            if let Some(handle) = HANDLE.get() {
                handle.set_input(OverlayInput::None);
                handle.fade_out(HIDE_FADE);
            }
        }
        publish_state(s, None);
        return;
    };
    let view = snap.view;
    s.private = snap.private;
    let fresh = s.fx.last.is_none() || s.fx.fading();
    if fresh {
        // 新一次组合重新判断候选窗朝向;候选行数变化时不会上下翻跳。
        s.fx.above = None;
        s.fx.slide = None;
        s.fx.expand = None;
        s.fx.page = None;
        s.fx.menu = None;
        s.fx.menu_hover = None;
        s.fx.hover = None;
        s.fx.chev = false;
        s.fx.since = Some(Instant::now());
    }
    s.fx.hide = None;
    let rect = caret_rect(s, &view);
    if rect.2 == 0 {
        // 独占全屏或拿不到光标信息:停靠窗口,并清掉冻结内容,
        // 下一次组合按新组合出现,不会接着上一次的状态。
        s.fx.hide = None;
        s.fx.slide = None;
        s.fx.expand = None;
        s.fx.last = None;
        park(s);
        return;
    }
    let now = Instant::now();
    let toggled = !fresh
        && s.fx
            .last
            .as_ref()
            .is_some_and(|last| last.expanded != view.expanded);
    if toggled && !s.fx.reduce && s.effects() {
        // 从当前窗口矩形(形变中就是当前帧)长到目标矩形。
        let from = s.fx.expand.map(|a| a.rect(now)).unwrap_or(s.fx.rest);
        let up = s.fx.above == Some(true);
        // 翻边(从光标下方跳到上方)不做高度形变:那会读成窗口瞬移。
        let same_side = if up {
            from.1 + from.3 == rect.1 + rect.3
        } else {
            from.1 == rect.1
        };
        s.fx.expand = (from.2 > 0 && from.3 > 0 && same_side).then(|| ExpandFx {
            from,
            to: rect,
            start: now,
            end: now + EXPAND_MS,
            up,
        });
        // 槽位重排后旧的悬停坐标已失效;箭头在棋盘里换了格子。
        s.fx.hover = None;
        s.fx.chev = false;
    } else if let Some(mut anim) = s.fx.expand {
        // 形变中内容变了:目标矩形跟上,动画继续走完。
        anim.to = rect;
        anim.up = s.fx.above == Some(true);
        s.fx.expand = Some(anim);
    }
    // 只有固定条翻页才滑动;展开棋盘里挪高亮不横向跳。
    if !s.fx.reduce
        && s.effects()
        && !view.expanded
        && s.fx.page.is_some_and(|page| page != view.page)
    {
        let previous = s.fx.page.unwrap_or(view.page);
        let direction = if view.page > previous { 1.0 } else { -1.0 };
        s.fx.slide = Some((direction, now + PAGE_SLIDE));
        s.fx.hover = None;
        s.fx.chev = false;
    }
    s.fx.page = Some(view.page);
    if view.promotions != s.fx.promotions {
        s.fx.promotions = view.promotions;
        if !s.fx.reduce && s.effects() {
            s.fx.flash = Some(now + PROMOTE_FLASH);
        }
    }
    s.fx.rest = rect;
    let laid = s.fx.expand.map(|a| a.rect(now)).unwrap_or(rect);
    layout(s, laid);
    // 先落内容再出帧;淡出期间用的就是这份冻结内容。
    s.fx.last = Some(view.clone());
    s.geom = Some(geom_of(s, &view));
    submit_scene(s, &view);
    publish_state(s, Some(&view));
    show(s);
    if let Some(handle) = HANDLE.get() {
        handle.set_input(OverlayInput::All);
    }
}

/// 光标位置算出的候选窗矩形(x, y, 宽, 高);宽为 0 表示这次不弹。
fn caret_rect(s: &mut Candidate, view: &SessionView) -> (i32, i32, i32, i32) {
    let (layout, ui_font, _) = place::ui_options(&s.state);
    let host_pid = (s.state.active.load(Ordering::Relaxed) >> 32) as u32;
    let Some((l, t, r, b)) = view.caret else {
        s.fx.above = None;
        s.state.candidate_above.store(false, Ordering::Relaxed);
        return (0, 0, 0, 0);
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
    // 同一次组合内优先留在已选的一侧;只有这一侧放不下才翻边。
    // 副屏可以位于主屏上方,`top` 是负数,不能直接 `.max(0)`。
    let gap = (place::CARET_GAP * scale) as i32;
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
    // 徽标据此躲到候选窗的另一侧,两者不互相遮挡。
    s.state
        .candidate_above
        .store(prefer_above, Ordering::Relaxed);
    let y =
        (if prefer_above { over } else { below }).clamp(work.top, (work.bottom - h).max(work.top));
    let x = left.clamp(work.left, (work.right - w).max(work.left));
    // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
    let hidden = place::foreground_fullscreen_at(left, b, host_pid);
    let w = if hidden { 0 } else { w };
    (x, y, w, h)
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
    s.logical = (rect.2 as f32 / s.scale, rect.3 as f32 / s.scale);
    *s.state
        .candidate_rect
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(rect);
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
        handle.set_placement(Some(placement_rect((x, y, 480, 40), s.scale)));
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

/// 动画帧:淡出/滑动/换首选提示推进中重绘;淡出到期停靠,其余到期走终态。
fn animate(s: &mut Candidate) {
    let now = Instant::now();
    if s.fx.hide.is_some_and(|end| now >= end) {
        s.fx.clear();
        park(s);
        return;
    }
    // 展开形变:窗口高度逐帧弹簧推进;到期落到精确目标矩形。
    if let Some(anim) = s.fx.expand {
        let rect = if now >= anim.end {
            s.fx.expand = None;
            anim.to
        } else {
            anim.rect(now)
        };
        layout(s, rect);
    }
    if s.fx.slide.is_some_and(|(_, end)| now >= end) {
        s.fx.slide = None;
    }
    if s.fx.flash.is_some_and(|end| now >= end) {
        s.fx.flash = None;
    }
}

/// 抓取窗口区的桌面像素作为玻璃背板;每帧重捕,背景变动跟随。
fn capture_backdrop(s: &mut Candidate, high_contrast: bool) {
    if !s.material.is_glass() || !s.glass_wanted(high_contrast) {
        s.backdrop = None;
        return;
    }
    let (x, y, w, h) = s.fx.rest;
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
    let (x, y, w, h) = s.fx.rest;
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
    let glass = s.material.is_glass() && s.glass_wanted(high_contrast) && s.backdrop.is_some();
    // 高透到磨砂的连续插值:压光与模糊同步加重。
    let frost = s.frost.clamp(0.0, 1.0);
    let frost_blur = GLASS_BLUR + (FROST_BLUR - GLASS_BLUR) * frost;
    let frost_fill = rgba(0x262624, 0.40 + 0.22 * frost);
    let frost_top = rgba(0x000000, 0.06 + 0.04 * frost);
    let frost_bottom = rgba(0x000000, 0.22 + 0.08 * frost);
    let (_, _, accent) = place::ui_options(&s.state);
    let palette = palette_of(s, accent, high_contrast);
    let now = Instant::now();
    let (dx, opacity) = list_fx(s, now);
    let scale = geom.metrics.scale();
    let (w, h) = s.logical;
    let mut root = el().w_full().h_full();
    if glass && let Some(pixels) = &s.backdrop {
        // 活背板:窗口区的桌面像素,blur 在其上出磨砂玻璃。
        root = root.child(el().w_full().h_full().pixels_fit(pixels, SurfaceFit::Fill));
    }
    let mut body = col()
        .absolute(0.0, 0.0)
        .w(w)
        .h(h)
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
                .border(1.0 * scale, rgba(0xffffff, 0.14))
                .shadow(8.0 * scale, 22.0 * scale, rgba(0x000000, 0.35));
        }
        // 白瓷:不透明暖象牙胎,釉面竖向明暗 + 瓷缘描边 + 柔影;不捕背板。
        (UiMaterial::Ceramic, _) if s.ceramic == UiCeramic::White => {
            body = body
                .gradient(rgb(0xf6f0e6), rgb(0xe4ddd0))
                .border(1.0 * scale, rgb(palette.border))
                .shadow(2.0 * scale, 16.0 * scale, rgba(0x3a3020, 0.30));
        }
        // 黑瓷:不透明暖黑胎,同构釉面。
        (UiMaterial::Ceramic, _) => {
            body = body
                .gradient(rgb(0x363029), rgb(0x28241d))
                .border(1.0 * scale, rgb(palette.border))
                .shadow(2.0 * scale, 16.0 * scale, rgba(0x000000, 0.40));
        }
        // 背板/动效不可用:实底主题兜底,绝不露原始桌面。
        _ => {
            body = body
                .fill(rgb(palette.bg))
                .border(1.0 * scale, rgb(palette.border))
                .shadow(2.0 * scale, 16.0 * scale, tint(0x000000, 56));
        }
    }
    let look = Look {
        palette: &palette,
        glass,
    };
    body = body.child(content(s, geom, &look, dx, opacity));
    root = root.child(body);
    if let Some((slot, _, text)) = s.fx.menu.as_ref().filter(|_| s.fx.expand.is_none())
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
        root = root.child(menu_tree(s, geom, &look, *slot));
    }
    root
}

/// 面板内容:固定条或展开棋盘,加右侧控制轨与条尾箭头。
fn content(s: &Candidate, geom: &Geom, look: &Look, dx: f32, opacity: f32) -> El {
    let meas: &dyn Measure = s;
    let scale = geom.metrics.scale();
    let mut layer = el().w_full().h_full();
    if geom.spec.expand && matches!(geom.spec.layout, Layout::Wrap) {
        // 棋盘:所有候选一样大,文字居中;控制轨收在最右。
        let cells = panel::grid_cells(&geom.spec, &geom.metrics, meas);
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
        // 固定条:自然宽度候选从左排到右;翻页滑动套在整层上。
        let rects = panel::item_rects(&geom.spec, &geom.metrics, meas);
        let mut strip = el()
            .absolute(dx, 0.0)
            .w(geom.width)
            .h(geom.height)
            .opacity(opacity);
        for (index, (item, rect)) in geom.spec.items.iter().zip(&rects).enumerate() {
            strip = strip.child(item_el(
                s,
                geom,
                look,
                item,
                *rect,
                index,
                s.fx.hover == Some(index),
            ));
        }
        let grid = panel::grid_rects(&geom.spec, &geom.metrics, meas);
        for (candidate, rect) in geom.spec.grid.iter().zip(&grid) {
            let budget = rect.width() - 8.0 * scale;
            let shown = panel::ellipsize(&candidate.text, budget.max(1.0), geom.metrics.text, meas);
            strip = strip.child(
                row()
                    .absolute(rect.left, rect.top)
                    .w(rect.width())
                    .h(rect.height())
                    .center_y()
                    .child(el().grow(1.0))
                    .child(
                        text(shown)
                            .font(TextStyle::new(geom.metrics.text, 400.0))
                            .color(rgb(look.palette.text)),
                    )
                    .child(el().grow(1.0)),
            );
        }
        layer = layer.child(strip);
    }
    layer = layer.child(control_tree(s, geom, look));
    layer
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
    hovered: bool,
) -> El {
    let scale = geom.metrics.scale();
    let mut cell = col()
        .absolute(rect.left, rect.top)
        .w(rect.width())
        .h(rect.height())
        .radius(rect.height() * 0.5)
        .center_y()
        .cursor(Cursor::Pointer)
        .on_hover_change(move |s: &mut Candidate, inside, _| {
            s.fx.hover = inside.then_some(index);
        })
        .press_on_down()
        .on_press(move |s: &mut Candidate| s.pick_slot(index));
    if item.selected {
        cell = selection_pill(cell, rect, look, scale);
    } else if hovered {
        cell = cell.fill(look.tint(look.palette.hover, 0x66));
    }
    let index_ink = if item.selected {
        look.palette.accent_text
    } else {
        look.palette.text_tertiary
    };
    cell = cell
        .child(
            el().absolute(panel::ITEM_PAD * scale, 0.0)
                .w(20.0 * scale)
                .center_y()
                .child(
                    text(format!("{}", index + 1))
                        .font(TextStyle::new(geom.metrics.small, 400.0))
                        .color(rgb(index_ink)),
                ),
        )
        .child(
            el().absolute(panel::TEXT_X * scale, 0.0).center_y().child(
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
            el().absolute(hint_x, 0.0)
                .w(100.0 * scale)
                .center_y()
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
                        s.scroll_grab_at(x, y);
                    }
                    rune::Phase::Move => {
                        let [_, y] = drag.window_point();
                        s.scroll_drag_to(y);
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
        let [a, b, c] = panel::chevron_points_turn(glyph, turn, scale);
        chev = chev
            .child(segment_el((a.x, a.y), (b.x, b.y), width, color))
            .child(segment_el((b.x, b.y), (c.x, c.y), width, color));
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
fn menu_tree(s: &Candidate, geom: &Geom, look: &Look, slot: usize) -> El {
    let meas: &dyn Measure = s;
    let (mx, my) = menu_origin(slot, &geom.spec, &geom.metrics, meas);
    let scale = geom.metrics.scale();
    let w = 116.0 * scale;
    let h = panel::menu_height(&geom.metrics);
    let mut card = col()
        .absolute(mx, my)
        .w(w)
        .h(h)
        .radius(10.0 * scale)
        .fill(rgb(look.palette.bg))
        .border(1.0 * scale, rgb(look.palette.border));
    for (index, label) in panel::MENU_ITEMS.iter().enumerate() {
        let y = (4.0 + index as f32 * 28.0) * scale;
        let hovered = s.fx.menu_hover == Some(index);
        let mut row = el()
            .absolute(4.0 * scale, y)
            .w((w - 8.0 * scale).max(0.0))
            .h(26.0 * scale)
            .radius(panel::concentric_radius(10.0 * scale, 4.0 * scale, 0.0))
            .px(4.0 * scale)
            .center_y()
            .cursor(Cursor::Pointer)
            .on_hover_change(move |s: &mut Candidate, inside, _| {
                s.fx.menu_hover = inside.then_some(index);
            })
            .press_on_down()
            .on_press(move |s: &mut Candidate| s.menu_click(slot, index));
        if hovered {
            row = row.fill(rgb(look.palette.hover));
        }
        card = card.child(
            row.child(
                text((*label).to_string())
                    .font(TextStyle::new(12.0 * scale, 400.0))
                    .color(rgb(if hovered {
                        look.palette.text
                    } else {
                        look.palette.text_secondary
                    })),
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

/// 翻页滑动与换首选提示的列表动效:(横向偏移, 透明度)。
fn list_fx(s: &Candidate, now: Instant) -> (f32, f32) {
    let scale = s.geom.as_ref().map_or(1.0, |geom| geom.metrics.scale());
    match (s.fx.slide, s.fx.flash) {
        (Some((direction, end)), _) if now < end => {
            let delta = ease_out_quint(progress(PAGE_SLIDE, end, now));
            (
                direction * 10.0 * scale * (1.0 - delta),
                0.55 + 0.45 * delta,
            )
        }
        (_, Some(end)) if now < end => {
            let delta = ease_out_quint(progress(PROMOTE_FLASH, end, now));
            (0.0, 0.72 + 0.28 * delta)
        }
        _ => (0.0, 1.0),
    }
}

impl Candidate {
    /// 发一条服务端请求;回落宿主没有 TSF 的 Sync,点完当场注入。
    fn request(&self, req: Request) {
        let _ = imp::handle(&self.state, req);
        imp::fallback::flush(&self.state);
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
        self.fx.menu = None;
        self.fx.menu_hover = None;
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
        self.fx.menu = Some((slot, 0.0, item.text.clone()));
        self.fx.menu_hover = None;
    }

    /// 回落钩子注入的鼠标事件;屏幕物理像素转客户区后走同一路径。
    fn on_fallback(&mut self, event: FallbackMouse, _tx: &mut Tx) {
        let (ox, oy) = (self.fx.rest.0 as f32, self.fx.rest.1 as f32);
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
            || menu_hover != self.fx.menu_hover
            || scroll_hover != self.fx.scroll_hover
        {
            self.fx.hover = item;
            self.fx.chev = chev;
            self.fx.menu_hover = menu_hover;
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
        if let (true, Some((slot, _, _))) = (self.fx.menu.is_some(), self.fx.menu.as_ref())
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
        let menu_hover = self
            .fx
            .menu
            .as_ref()
            .filter(|_| self.fx.expand.is_none())
            .and_then(|(slot, _, _)| {
                let (mx, my) = menu_origin(*slot, &geom.spec, &geom.metrics, meas);
                panel::menu_hit(mx, my, &geom.metrics, x, y)
            });
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
    let mut cell = col()
        .absolute(rect.left, rect.top)
        .w(rect.width())
        .h(rect.height())
        .radius(rect.height() * 0.5)
        .center_y()
        .px(8.0 * scale)
        .cursor(Cursor::Pointer)
        .press_on_down()
        .on_press(move |s: &mut Candidate| s.pick_index(index));
    if selected {
        cell = selection_pill(cell, rect, look, scale);
    }
    cell.child(
        row()
            .w_full()
            .center_y()
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
