//! 候选窗:Rune overlay 承载窗口、玻璃与输入;布局、命中与分页语义
//! 仍来自 `lueying_render::panel`,摆位共用 `ui::place`。
//!
//! 玻璃走自捕获背板:逐帧 BitBlt 窗口区的桌面像素作为背景图,面板
//! `backdrop_blur` 出磨砂玻璃;瓷面不透明、不抓背板,釉光来自面板
//! 上下窄带的环境采样。「动态效果」关掉时回退实底主题。窗口
//! 移动、隐藏与淡出全走 Rune 的 OverlayHandle;管道线程经版本号 +
//! `kick()` 唤醒本线程,鼠标回落事件经 `forward_mouse` 注入同一队列。
//!
//! placement 的 Rect 相对主屏原点(主屏缩放换算);尺寸与内容按光标
//! 所在屏的缩放走。混合 DPI 副屏上的渲染锐度是已知限制,主屏与等比
//! 多屏下位置、尺寸、内容三者一致。

use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use lueying_protocol::{Request, SessionView};
use lueying_render::panel::{self, Item, Layout, Measure, Metrics, PanelSpec, Rect};
use rune::tok::motion;
use rune::{
    Align, App, AppSpec, Color, Cursor, Edge, El, LineCap, Live, OverlayHandle, OverlayInput,
    OverlayOptions, OverlayPlacement, OverlaySpec, Path, Pixels, Shape, Side, Spring, SurfaceFit,
    TextStyle, Transition, Tx, anchor_rect, canvas, col, el, text,
};
use rune_text::TextSystem;

use lueying_server::config::{UiLayout, UiMaterial};

use crate::imp;
use crate::liquid;
use crate::ui::place;

/// 活背板的重捕节奏(~30fps):背景变动跟随,再高肉眼无感只费电。
const LIVE_TICK: Duration = Duration::from_millis(33);
/// 瓷釉环境采样的重采节奏:周边变了釉光跟着变,又不必像玻璃那样逐帧跟。
const AMBIENT_TICK: Duration = Duration::from_millis(500);
/// 候选文字最大宽度(逻辑像素):窗口宽度减去页码与翻页箭头的空间。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 组合结束后的淡出时长;期间沿用最后一份内容并让鼠标穿透。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 可疑左移光标的复核窗口:盖过两帧,期间有更新的报告就作废。
const CARET_HOLD: Duration = Duration::from_millis(40);

/// 磨砂玻璃的模糊强度:更重,接近亚克力;高透端的 σ 由变体给出。
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

/// 候选窗的存在状态;截止时刻只在淡出中有效。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Presence {
    /// 无淡出挂起;显示与否由窗口可见性与冻结内容决定。
    #[default]
    Idle,
    /// 原生 alpha 淡出进行中,到期停靠;期间沿用冻结内容并让鼠标穿透。
    FadingOut(f64),
}

/// 右键菜单状态;悬停槽位只在打开时有效。
#[derive(Clone, Debug, Default, PartialEq)]
enum Menu {
    #[default]
    Closed,
    /// (全局候选序号, 悬停项, 行文本)。行文本变了菜单自动失效。
    Open {
        gidx: usize,
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
    /// 最后一份组合内容;淡出期间冻结,平时跟最新一帧。与引擎的
    /// 发布队列同住一份 Arc,不逐帧克隆候选列表。
    last: Option<Arc<imp::Published>>,
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
    since: Option<f64>,
    /// 本帧的逻辑钟(poll 的 t,秒):液块采样、淡出进度、捕获节流同走
    /// 一口钟,headless 下与虚拟时钟一致,不再各摸各的墙钟。
    now: f64,
    /// 挂起中的可疑光标:锚点左移的报告挂一拍复核,到期无人覆盖才应用。
    caret_hold: Option<CaretHold>,
    /// 选中几何的目标矩形(面板本地):原生 lead 弹簧的目标,绘制轮廓
    /// 从它和逐帧绘制矩形取形。
    sel_to: Option<Rect>,
    /// 原生 lead 上一帧写出的绘制矩形(窗口点)与时刻:判变化、算尾速。
    sel_drawn_last: [f32; 4],
    sel_drawn_at: f64,
    /// 本提交选中几何直接落位(页变/形变/拖动/减少动态):轮廓用目标
    /// 矩形,不等下一帧的绘制矩形。
    sel_snap: bool,
    /// 面板在窗口里的逻辑原点:绘制矩形是窗口点,轮廓与目标都是面板
    /// 本地,换算用这一份。
    sel_origin: (f32, f32),
    /// 选中几何的主轴(true=x)与行进方向(+1 右/下,-1 左/上)。
    sel_axis_x: bool,
    sel_forward: f32,
    /// 聚合余震:高速归位后胶囊按衰减正弦抖(水滴落地)。瞬时与振幅。
    sel_wobble: Option<(f64, f32)>,
    /// 选中几何尾长变化的全程峰值:聚合瞬间转成余震能量
    /// (停稳判定要求速度≈0,峰值必须另记)。
    sel_peak_v: f32,
    /// 上一份内容的 sel 矩形(逻辑点,面板本地);比 view 数据可靠——
    /// 「我→我们」这类纯文字替换也会让胶囊变宽,view.highlight 不变。
    sel_rect: Option<Rect>,
}

/// 棋盘滑块的果冻弹簧:后缘(相对行进方向,下行是上沿)走更慢的
/// 欠阻尼弹簧——行进中它滞后,滑块被拉长;前缘停稳后它才过冲,
/// 就是撞击那瞬的单边压缩,随后回弹复圆。后缘若比前缘快,行进中
/// 就会提前过冲,变成「两边往中间挤」。交给元素布局动画
/// (rt.animating 车道)跑,帧必到,不押在会被 overlay 宿主滤掉
/// 的定时器唤醒上。
const THUMB_LEAD: rune::Spring = rune::Spring::new(0.30, 0.0);
const THUMB_TRAIL: rune::Spring = rune::Spring::new(0.50, 0.5);

/// 选中几何走 Rune 原生 lead(前缘 motion::LEAD,后缘 motion::DEFAULT):
/// 单步轻点只有一条短瞬态,连按才把后缘落下。这里只剩物理封顶与
/// 换水滴轮廓的门槛,不再按热度/速度分档。
/// 液块封顶:尾长最多 3.5×半高(表面张力,不能被无限拉长)。
const GOO_CAP: f32 = 3.5;
/// 拉伸超过 0.7×头径才换成水滴轮廓;短拉伸保持胶囊,尖头不露怯。
const GOO_DROP_RATIO: f32 = 0.7;
/// 甩珠的尾速门槛(逻辑点/秒):只有真连击才甩。
const GOO_SHED_V: f32 = 300.0;

impl Fx {
    fn clear(&mut self) {
        *self = Fx::default();
    }

    fn fading(&self) -> bool {
        matches!(self.presence, Presence::FadingOut(_))
    }
}

/// 0..1 的动画进度;已过期钳到 1。
fn progress(total: Duration, end: f64, now: f64) -> f32 {
    let remain = Duration::from_secs_f64((end - now).max(0.0));
    (1.0 - remain.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

fn ease_out_quint(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(5)
}

/// 一帧的视觉参数:调色板、灯色与高对比度;控件树的各构件共用。
struct Look<'a> {
    palette: &'a panel::Palette,
    /// 选中胶囊的灯色:胶囊自己是面板的光源,灯色跟主题色走。
    glow: lueying_server::theme::Glow,
    /// 亮芯上选中文字的墨:高对比度用系统选中字色,其余用灯色的深色同源。
    selected_ink: Color,
    high_contrast: bool,
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
    /// 固定条按内容自适应放下的条数;变化时回喂引擎当页大小。
    fit: usize,
}

/// 背板像素的流式通道;原图去重在 `capture_backdrop` 的 `backdrop_raw` 已做,
/// 这里不再存一份比。调用方只在内容变了时 publish。
struct Backdrop {
    pixels: Pixels,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self {
            pixels: Pixels::new(),
        }
    }
}

impl Backdrop {
    fn publish(&mut self, width: u32, height: u32, rgba: Vec<u8>) -> bool {
        self.pixels.set(width, height, rgba);
        true
    }
}

/// 候选窗 overlay 的状态;Rune 每帧从它重建控件树。
struct Candidate {
    state: Arc<imp::State>,
    fx: Fx,
    /// 文本测量;候选条的自然宽度在显示前就要量好。
    ink: RefCell<TextSystem>,
    /// 玻璃背板:流式像素通道,一次分配,逐帧 `set` 原地换桌面像素。
    backdrop: Option<Backdrop>,
    backdrop_read: Option<(f64, (i32, i32, i32, i32))>,
    /// 上一次喂给光学的原始背板:静止时逐位一致,跳过重算与上传。
    backdrop_raw: Option<((u32, u32), Vec<u8>)>,
    /// 亚克力颗粒:面板尺寸的固定噪点图;随尺寸重生成。
    grain: Option<Pixels>,
    grain_size: Option<(u32, u32)>,
    /// 釉光的环境光:面板上下窄带的桌面实采均值,瓷釉光泽与玻璃棱缘
    /// rim 共用,没有来源就不上光。
    glaze: Option<lueying_server::theme::Glaze>,
    glaze_read: Option<(f64, (i32, i32, i32, i32))>,
    /// 外观扩展的主题色覆盖;None 用系统主题。
    script_accent: Option<u32>,
    /// 上一次见到的视图版本号。
    version: u64,
    /// 候选窗材质;配置重载后随版本号生效。
    material: UiMaterial,
    /// 玻璃磨砂程度 0..=1;高透到磨砂连续可调。
    frost: f32,
    /// 玻璃变体(临时调参);配置重载后随版本号生效。
    glass: liquid::GlassVariant,
    glass_name: String,
    /// 生效的折射带宽:变体默认或 `ui_glass_band` 覆盖值(临时调参)。
    glass_band: f32,
    /// 配置里的主题色下标;变化时让调色板用弹簧渐变过去。
    accent: usize,
    /// 私密字段标记;只影响扩展发布,绘制照常。
    private: bool,
    visible: bool,
    /// 光标屏缩放;placement 与内容共用。
    scale: f32,
    /// 选中几何的逐帧绘制矩形(窗口点):原生 lead 弹簧画完每帧写回,
    /// 轮廓据此重建,弹簧本体不再手采样。
    sel_drawn: Live<[f32; 4]>,
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
            backdrop_read: None,
            backdrop_raw: None,
            grain: None,
            grain_size: None,
            glaze: None,
            glaze_read: None,
            script_accent: None,
            version: 0,
            material: UiMaterial::Glass,
            frost: 0.0,
            glass: liquid::variant("A"),
            glass_name: "A".to_string(),
            glass_band: 8.0,
            accent: 0,
            private: false,
            visible: false,
            scale: 1.0,
            sel_drawn: Live::new([0.0; 4]),
            geom: None,
            sent: None,
            comp: 0,
        }
    }

    /// 命中交互是否可用:激活会话且候选窗在显示。
    fn interactive(&self) -> bool {
        self.state.active.load(Ordering::Relaxed) != 0
            && self
                .state
                .active_published()
                .is_some_and(|published| published.composing && !published.host_draws)
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

    /// 釉面环境采样是否会被画:高对比度让位系统色;玻璃私密态与实底
    /// 兜底都不读釉光,瓷与高透玻璃照采。淡出期保留采样不闪变。
    fn glaze_wanted(&self, high_contrast: bool) -> bool {
        if high_contrast {
            return false;
        }
        match self.material {
            UiMaterial::Ceramic => true,
            UiMaterial::Glass => self.glass_wanted(high_contrast) && !self.private,
        }
    }

    fn live_glass(&self, high_contrast: bool) -> bool {
        !self.private
            && self.material.is_glass()
            && self.visible
            && !self.fx.fading()
            && self.glass_wanted(high_contrast)
    }

    /// 组合结束后的外观时钟;减少动态效果时恒为 1。
    fn clock(&self, now: f64) -> f32 {
        if self.fx.reduce {
            return 1.0;
        }
        self.fx
            .since
            .map_or(1.0, |start| (now - start).max(0.0) as f32)
    }

    /// 淡出分支的进度;静止恒为 1。
    fn anim(&self, now: f64) -> f32 {
        match self.fx.presence {
            Presence::FadingOut(end) => ease_out_quint(progress(HIDE_FADE, end, now)).max(0.001),
            Presence::Idle => 1.0,
        }
    }

    /// 当前页在候选列表里的起点(全局序号);条内槽位与全局序号互算用。
    fn page_start(&self) -> usize {
        self.fx.last.as_ref().map_or(0, |last| {
            panel::page_bounds(
                last.view.page,
                last.view.page_size,
                last.view.candidates.len(),
            )
            .start
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

/// 0xRRGGBB → Rune 颜色(线性 sRGB);转换共享一份,见 theme::srgb。
fn rgb(v: u32) -> Color {
    lueying_server::theme::srgb(v)
}

/// 0xRRGGBB + 线性透明度。
fn rgba(v: u32, a: f32) -> Color {
    lueying_server::theme::srgb_alpha(v, a)
}

/// 旧画布的 8bit alpha → 0..1。
fn tint(v: u32, alpha: u8) -> Color {
    rgba(v, f32::from(alpha) / 255.0)
}

/// 文本在 h 高容器里垂直居中的 y。Rune 的 center_y 居中的是行盒,
/// CJK 墨迹在行盒里天然偏上;按实机帧标定的墨迹几何补偿。
/// 工具栏与选择面板同一套 CJK 基准,共用这里。
pub(super) fn ink_y(h: f32, size: f32) -> f32 {
    h * 0.5 - (INK_TOP + INK_HEIGHT * 0.5) * size
}

/// 液态玻璃的中性面:烟熏玻璃配暖白墨与暖边框。强调族(灯色/序号/
/// 选中)跟主题色走,胶囊是光源,不在墨里固定颜色。
fn glass_ink(mut p: panel::Palette) -> panel::Palette {
    p.text = 0xece7df;
    p.text_secondary = 0xe3ddd2;
    p.text_tertiary = 0xd4cec3;
    p.border = 0x4a4238;
    p.hover = 0x3a332a;
    p
}

/// 当前帧的调色板:玻璃固定暖墨烟熏,瓷走共享的暖黑釉,实底跟系统主题。
/// 高对比度一律让位系统色,材质的墨色覆盖全部跳过。
fn palette_of(s: &Candidate, accent: usize, high_contrast: bool) -> panel::Palette {
    let look = lueying_server::theme::Appearance::Dark;
    let colors = match s.script_accent {
        Some(v) if !high_contrast => lueying_server::theme::colors_with_accent(look, v),
        _ => lueying_server::theme::colors_with(look, accent),
    };
    let palette = lueying_server::theme::palette(colors);
    match s.material {
        // 瓷:固定暖黑胎,强调族跟主题色;高对比度让位系统色。
        UiMaterial::Ceramic if !high_contrast => ceramic_ink(palette),
        UiMaterial::Glass if !high_contrast => glass_ink(palette),
        _ => palette,
    }
}

/// 黑瓷的中性面:暖黑胎配暖白墨。强调族(灯色/序号/选中)跟主题色,
/// 胶囊是光源,不在墨里固定颜色。
fn ceramic_ink(mut p: panel::Palette) -> panel::Palette {
    p.bg = 0x2e2a24;
    p.window_bg = 0x2e2a24;
    p.surface_alt = 0x262220;
    p.border = 0x4a4238;
    p.border_strong = 0x554c3e;
    p.text = 0xece7df;
    p.text_secondary = 0xe3ddd2;
    p.text_tertiary = 0xd4cec3;
    p.hover = 0x3a332a;
    p
}

/// 入口:建 Rune 应用与 overlay;事件循环不返回。
pub(crate) fn run(state: Arc<imp::State>) {
    let spec = AppSpec {
        families: ("Noto Sans SC", "Noto Sans SC"),
        // App::init 完全覆盖 AppSpec::init(上游 into_parts 只跑其一):fn 指针
        // 是必填占位,永不执行,不再经全局转交状态。
        ..AppSpec::new(
            "lueying-rune-candidate",
            (480.0, WRAP_WINDOW_H),
            dead_init,
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
    let overlay = overlay
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
            exclude_from_capture: true,
            ..Default::default()
        })
        // 隐藏即归还 GPU 表面(上游 release_gpu_when_hidden):复显时 surface 重建,
        // 首帧必是新内容,不闪上一组句的候选页或展开棋盘;device 仍按上游策略保留
        // 五分钟,复显只等几毫秒,不触发管线重建。(prewarm 与它互斥,预热效果
        // 留不住,不配。)
        .release_gpu_when_hidden()
        .poll(poll);
    let handle = app.overlay(overlay, Candidate::with_state(state.clone()));
    let _ = HANDLE.set(handle);
    // 模式徽标:同一 App 的第二个 overlay,隐藏期不占窗口与表面。
    super::badge::register(&mut app, Arc::clone(&state));
    // 工具栏:第三个 overlay;宿主选择面板:普通窗口,按需打开。
    // 两者的状态字与请求都经各自的 poll 读取,管道线程只管叫醒。
    super::toolbar::register(&mut app, Arc::clone(&state));
    let picker_jobs = super::picker::install(&state);
    let (picker_app, picker_window) = app.window(super::picker::window_spec(picker_jobs));
    super::picker::attach(picker_app, picker_window);
    app.run();
}

/// AppSpec 必填的 init 占位:App::init 在上游 into_parts 里完全覆盖它,永不执行。
fn dead_init() -> Candidate {
    unreachable!("App::init 已接管状态构建")
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
/// 事件没改任何视图状态时(update_if 契约)不重跑 view,悬停扫过
/// 已悬停的行不再整树重建。
pub(crate) fn forward_mouse(event: FallbackMouse) {
    if let Some(handle) = HANDLE.get() {
        handle.update_if(move |s: &mut Candidate, tx| s.on_fallback(event, tx));
    }
}

/// 每帧开头跑一次:补丁、版本、摆位、活背板,并决定下一次唤醒。
fn poll(s: &mut Candidate, now: f64, tx: &mut Tx) -> bool {
    // 全窗一口钟:poll 的 t 是 Rune 帧钟,view 里所有采样都读它。
    s.fx.now = now;
    // 原生 lead 的选中几何:每帧读画完写回的绘制矩形,变了就重跑
    // 视图重建水滴轮廓;弹簧重定向与速度接力由 Rune 自己保,不再手
    // 采样四条边。只标记本轮要重绘并保住下一帧唤醒,绝不能提前
    // return——版本检查与 place() 都在后面,跳过了新按键要等动画
    // 停稳才被看见("按了顿住没反应,打断像没做"实测就是这个)。
    let snap = s.fx.sel_snap;
    s.fx.sel_snap = false;
    let mut sel_live = false;
    let drawn = s.sel_drawn.get();
    if drawn[2] > 0.0 && drawn != s.fx.sel_drawn_last {
        if snap {
            // 直接落位那一拍:旧矩形到目标是一步跳,不能当尾速记,
            // 否则页变/形变会凭空抖一下。
            s.fx.sel_peak_v = 0.0;
        } else {
            let dt = (now - s.fx.sel_drawn_at).max(1.0 / 240.0) as f32;
            let main = |r: [f32; 4]| if s.fx.sel_axis_x { r[2] } else { r[3] };
            let v = (main(drawn) - main(s.fx.sel_drawn_last)) / dt;
            s.fx.sel_peak_v = s.fx.sel_peak_v.max(v.abs());
        }
        s.fx.sel_drawn_last = drawn;
        s.fx.sel_drawn_at = now;
        tx.wake_after(Duration::ZERO);
        sel_live = true;
    }
    // 原生弹簧停稳且尾速够狠:聚合余震(衰减正弦抖高)。慢步不抖。
    if let Some(target) = s.fx.sel_to
        && s.fx.sel_wobble.is_none()
        && drawn[2] > 0.0
        && (drawn[0] - (target.left + s.fx.sel_origin.0)).abs() < 0.5
        && (drawn[1] - (target.top + s.fx.sel_origin.1)).abs() < 0.5
        && (drawn[2] - target.width()).abs() < 0.5
        && (drawn[3] - target.height()).abs() < 0.5
    {
        let amp = (s.fx.sel_peak_v * 0.0004).min(0.30);
        if amp > 0.08 {
            s.fx.sel_wobble = Some((now, amp));
        }
        s.fx.sel_peak_v = 0.0;
    }
    if let Some((w0, _)) = s.fx.sel_wobble {
        if (now - w0) as f32 > 0.55 {
            s.fx.sel_wobble = None;
        } else {
            tx.wake_after(Duration::ZERO);
            sel_live = true;
        }
    }
    let appearance = lueying_server::theme::Appearance::of_system();
    let high_contrast = matches!(appearance, lueying_server::theme::Appearance::HighContrast);
    // 外观补丁:晚到的旧补丁直接丢;主题色变了就重绘,颜色用弹簧渐变过去。
    if let Some((seq, patch)) = s.state.ext.take_patch()
        && seq >= s.state.ext.scene_seq()
    {
        let accent = patch.and_then(|patch| patch.accent);
        if accent != s.script_accent {
            s.script_accent = accent;
            tx.color_motion(motion::CALM);
        }
    }
    let mut changed = false;
    let version = s.state.version.load(Ordering::Acquire);
    if version != s.version {
        s.version = version;
        s.fx.reduce = !lueying_server::theme::client_area_animation();
        let cfg = s.state.config.read().unwrap_or_else(|p| p.into_inner());
        let material = cfg.ui_material;
        let frost = cfg.ui_glass_frost.clamp(0.0, 1.0);
        let accent = cfg.ui_accent;
        // 带宽/变体变了要让下一轮捕获重算光学:原图去重只看像素,
        // 参数变了它会把旧结果一直留下去(折射「消失」的一半来源)。
        let mut glass_dirty = false;
        if s.glass_name != cfg.ui_glass_variant {
            s.glass_name = cfg.ui_glass_variant.clone();
            s.glass = liquid::variant(&cfg.ui_glass_variant);
            glass_dirty = true;
        }
        let band = if cfg.ui_glass_band > 0.0 {
            cfg.ui_glass_band
        } else {
            s.glass.band
        };
        if (s.glass_band - band).abs() > f32::EPSILON {
            glass_dirty = true;
        }
        s.glass_band = band;
        if glass_dirty {
            s.backdrop_raw = None;
        }
        drop(cfg);
        // 配置换了外观:调色板与材质参数走官方的主题变化弹簧渐变过去。
        if s.material != material || s.frost != frost || s.accent != accent {
            s.material = material;
            s.frost = frost;
            s.accent = accent;
            tx.color_motion(motion::CALM);
        }
        place(s, tx);
        // The standalone measurer is not driven by Runtime's layout commits.
        s.ink.get_mut().end_commit();
        changed = true;
    }
    // 淡出到期停靠;摆位时已排好这一次唤醒。
    if matches!(s.fx.presence, Presence::FadingOut(end) if now >= end) {
        s.fx.clear();
        park(s);
        return true;
    }
    if s.live_glass(high_contrast) {
        let rect = s.fx.placement.panel;
        if s.backdrop_read
            .is_none_or(|(last, area)| area != rect || now - last >= LIVE_TICK.as_secs_f64())
        {
            s.backdrop_read = Some((now, rect));
            changed |= capture_backdrop(s);
        }
        if let Some((last, _)) = s.backdrop_read {
            tx.wake_at(last + LIVE_TICK.as_secs_f64());
        }
    } else if s.private || !s.glass_wanted(high_contrast) || !s.material.is_glass() {
        s.backdrop_read = None;
        changed |= s.backdrop.take().is_some();
    }
    // 釉光的环境光:面板上下窄带低频实采,周边变了光跟着变。瓷釉的
    // 光泽与玻璃的棱缘 rim 都吃这份采样;淡出期保留,冻结内容里不
    // 闪变;停靠由 park 清。会被画的材质才采,实底主题与私密玻璃不采。
    if s.visible && s.glaze_wanted(high_contrast) {
        let rect = s.fx.placement.panel;
        if s.glaze_read
            .is_none_or(|(last, area)| area != rect || now - last >= AMBIENT_TICK.as_secs_f64())
        {
            s.glaze_read = Some((now, rect));
            changed |= capture_glaze(s);
        }
        if let Some((last, _)) = s.glaze_read {
            tx.wake_at(last + AMBIENT_TICK.as_secs_f64());
        }
    } else {
        changed |= s.glaze.take().is_some();
    }
    changed | sel_live
}

/// 组合结束或没有候选可画:reduce 直接停靠;否则窗口留在原地淡出,
/// 到期由轮询停靠。后续键补上内容时,fresh 会取消排队中的 Hide。
fn retract(s: &mut Candidate, tx: &mut Tx) {
    if s.fx.reduce || s.fx.last.is_none() {
        s.fx.clear();
        park(s);
        return;
    }
    if !s.fx.fading() {
        s.fx.presence = Presence::FadingOut(s.fx.now + HIDE_FADE.as_secs_f64());
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
}

/// 一轮摆位:脚本补丁到了就应用;版本变了就摆位与出内容。
fn place(s: &mut Candidate, tx: &mut Tx) {
    let Some(published) = place::snapshot(&s.state) else {
        retract(s, tx);
        return;
    };
    let view = &published.view;
    s.private = published.private;
    // 组合在但没有候选可画(裸 v/u 的表达式与问字空档、v1x 打到运算
    // 符还没出结果):空条渲染出来就是一根牙签。按组合结束同一条路径
    // 收起;键补上候选时 fresh 取消排队中的 Hide,候选窗正常出现。
    if view.candidates.is_empty() {
        retract(s, tx);
        return;
    }
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
        s.fx.caret_hold = None;
        s.fx.menu = Menu::Closed;
        s.fx.hover = None;
        s.fx.chev = false;
        s.fx.since = Some(s.fx.now);
        // 入场过渡只在 animated 提交里播;这一条提交把它打出来。
        tx.animated();
    }
    s.fx.presence = Presence::Idle;
    // 挂起中的可疑光标:被更新的报告顶掉就作废;复核到期仍是同一份,
    // 说明是持久移动(折行、滚动),放行走常规摆位。
    let mut holding = s.fx.caret_hold.take();
    let mut released = false;
    if let Some(hold) = holding {
        if view.caret != Some(hold.rect) {
            holding = None;
        } else if s.fx.now - hold.at >= CARET_HOLD.as_secs_f64() {
            holding = None;
            released = true;
        }
    }
    // 组合没变短而锚点左移:多半是宿主排版中途的陈旧测量,退化宿主还
    // 会落到组合行框或视图左缘,直接上屏就是打字时偶发的一帧左抽。挂
    // 起一拍:期间更新的报告照常覆盖;没人覆盖就按持久移动放行。
    let mut fresh_hold = None;
    if holding.is_none()
        && !released
        && !fresh
        && let Some(new) = view.caret
        && let Some((old, old_len)) =
            s.fx.last
                .as_ref()
                .and_then(|last| last.view.caret.map(|caret| (caret, last.view.raw.len())))
        && new.0.min(new.2) < old.0.min(old.2)
        && view.raw.len() >= old_len
    {
        fresh_hold = Some(CaretHold {
            rect: new,
            at: s.fx.now,
        });
        tx.wake_after(CARET_HOLD);
    }
    s.fx.caret_hold = fresh_hold.or(holding);
    // 绘制几何只算一份:摆位(宽高)、命中与控件树共用。挂起回合用
    // 沿用摆位对应的缩放;正常回合先按新光标换缩放再量。
    if s.fx.caret_hold.is_none()
        && let Some((l, _, r, b)) = view.caret
    {
        s.scale = place::scale_at(l.min(r), b);
    }
    let geom = geom_of(s, view);
    // 选中矩形变化(换高亮、文字变宽、条↔棋盘同位重排):交给原生
    // lead——前缘/后缘各一条弹簧,重定向保速度;轮廓在 content 里
    // 按逐帧绘制矩形取形。页变/形态变各有推入与形变,选中几何直接
    // 落位,不跨形态拉一条旧坐标的桥。
    let cur_sel = sel_rect_of(s, &geom);
    let prev_sel = s.fx.sel_rect;
    let same_form =
        s.fx.last
            .as_ref()
            .is_some_and(|last| last.view.page == view.page && last.view.expanded == view.expanded);
    if cur_sel != prev_sel {
        s.fx.sel_to = cur_sel;
        s.fx.sel_drawn_at = s.fx.now;
        if let (Some(prev), Some(cur)) = (prev_sel, cur_sel) {
            let dx = (cur.left + cur.width() / 2.0) - (prev.left + prev.width() / 2.0);
            let dy = (cur.top + cur.height() / 2.0) - (prev.top + prev.height() / 2.0);
            s.fx.sel_axis_x = dx.abs() >= dy.abs();
            let d = if s.fx.sel_axis_x { dx } else { dy };
            s.fx.sel_forward = if d < 0.0 { -1.0 } else { 1.0 };
        }
        if s.effects() && !fresh && s.fx.scroll_drag.is_none() && same_form {
            tx.animated();
        } else {
            s.fx.sel_snap = true;
            s.fx.sel_peak_v = 0.0;
        }
    }
    s.fx.sel_rect = cur_sel;
    let placement = if s.fx.caret_hold.is_some() {
        // 挂起回合:沿用上一回合摆位,内容照常更新,到期复核再挪。
        s.fx.placement
    } else {
        rect_for_caret(s, view, &geom)
    };
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
            .is_some_and(|last| last.view.expanded != view.expanded);
    if toggled {
        tx.animated();
        // 槽位重排后旧的悬停坐标已失效;箭头在棋盘里换了格子。
        s.fx.hover = None;
        s.fx.chev = false;
        // 形态变:选中几何直接落在新形态的目标格,不跨旧棋盘坐标
        // 拉一条桥(那会在胶囊附近画出一块蓝色残块)。
        s.fx.sel_snap = true;
    }
    // 只有固定条翻页才滑动;展开棋盘里挪高亮不横向跳。上一帧也得是
    // 固定条:展开→收起那拍页码常变,武装了只会在收起后的重挂载里
    // 回放。武装单拍有效:下一提交页码没再变就清掉,不然残留期间
    // 任何视口重挂载(液块水珠增减都会顶动它)都把推入再播一遍,
    // 读作元素突然从右缘飞回——收起收尾时的「右飞」就是这个。
    if s.effects()
        && !view.expanded
        && s.fx
            .last
            .as_ref()
            .is_some_and(|last| !last.view.expanded && last.view.page != view.page)
    {
        let previous = s.fx.last.as_ref().map_or(view.page, |last| last.view.page);
        s.fx.slide_edge = Some(if view.page > previous {
            Edge::Trailing
        } else {
            Edge::Leading
        });
        tx.animated();
        s.fx.hover = None;
        s.fx.chev = false;
        // 页间格子整批换人,选中几何两端都已失效,同上直接落位。
        s.fx.sel_snap = true;
    } else {
        s.fx.slide_edge = None;
    }
    // 同形态、同页内选中迁移:液桥取代元素弹簧(上面已按矩形变化
    // 起 goo);页变走推入、形态变走形变,各自已 animated。
    // 棋盘滚动起点变化:animated 提交让滑块沿布局弹簧行进去——行进
    // 时后缘欠阻尼,撞端过冲即压缩、回弹复原。poll 引发的提交默认
    // 全部即时落地,必须显式要动画,否则滑块瞬移而原生果冻不播。
    if s.effects()
        && !fresh
        && s.fx.last.as_ref().is_some_and(|last| {
            last.view.grid_scroll != view.grid_scroll && last.view.expanded && view.expanded
        })
    {
        tx.animated();
    }
    // 面板整体挪动时,上一帧的绘制矩形还带着旧窗口原点;轮廓这一拍
    // 直接落目标,避免一帧错位。
    s.fx.sel_origin = (
        (placement.panel.0 - placement.window.0) as f32 / geom.metrics.scale(),
        (placement.panel.1 - placement.window.1) as f32 / geom.metrics.scale(),
    );
    if (placement.panel.0, placement.panel.1) != (s.fx.placement.panel.0, s.fx.placement.panel.1) {
        s.fx.sel_snap = true;
    }
    s.fx.placement = placement;
    // 回落钩子按面板矩形转发点击;发布给扩展的也是它。
    *s.state
        .candidate_rect
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = Some(placement.panel);
    layout(s, placement.window);
    submit_scene(s, view);
    publish_state(s, Some(view));
    // 先落内容再出帧;淡出期间用的就是这份冻结内容,与发布队列共享一份。
    s.geom = Some(geom);
    s.fx.last = Some(published);
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

/// 一份待复核的光标矩形(屏幕物理像素)与挂起起点。
#[derive(Clone, Copy)]
struct CaretHold {
    rect: (i32, i32, i32, i32),
    at: f64,
}

/// 光标位置算出的摆位;宽为 0 表示这次不弹。
/// 横排窗口恒为展开棋盘的尺寸,条贴光标侧,其余区域透明且指针穿透;
/// 竖排窗口与面板同矩形。几何(宽高、自适应条数)取摆位前算好的那
/// 一份,不再重测。
fn rect_for_caret(s: &mut Candidate, view: &SessionView, geom: &Geom) -> Placement {
    let (layout, _, _) = place::ui_options(&s.state);
    let host_pid = (s.state.active.load(Ordering::Relaxed) >> 32) as u32;
    let Some((l, t, r, b)) = view.caret else {
        s.fx.above = None;
        s.state.candidate_above.store(false, Ordering::Relaxed);
        return Placement::default();
    };
    let left = l.min(r);
    let s_factor = geom.metrics.scale();
    if geom.fit != view.page_size {
        let session = s.state.active.load(Ordering::Relaxed);
        let _ = imp::handle(
            &s.state,
            Request::SetPageSize {
                session,
                size: geom.fit,
            },
        );
    }
    let mut w = geom.width as i32;
    let h = geom.height as i32;
    let work = place::work_area_at(left, b);
    w = w.min((work.right - work.left).max(1));
    let gap = (place::CARET_GAP * s_factor) as i32;
    // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
    let hidden = place::foreground_fullscreen_at(left, b, host_pid);
    if hidden {
        return Placement::default();
    }
    let horizontal = matches!(layout, UiLayout::Horizontal);
    if horizontal {
        // 横排:窗口恒为棋盘高度;条贴光标侧,展开向上/向下长满整窗。
        let h_win = (WRAP_WINDOW_H * s_factor) as i32;
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

/// 当前视图的绘制几何;摆位、命中测试与控件树共用同一份。
fn geom_of(s: &mut Candidate, view: &SessionView) -> Geom {
    let (layout, ui_font, _) = place::ui_options(&s.state);
    let scale = s.scale;
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let (mut spec, fit) = place::spec_of(view, layout, &metrics, s);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = place::measure_width(view, layout, &metrics, s);
    Geom {
        width: panel::panel_width(&spec, &metrics, s),
        height: panel::panel_height(&spec, &metrics, s),
        spec,
        metrics,
        fit,
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

/// 物理矩形 → placement 的主屏本地逻辑坐标;徽标与候选窗共用。
pub(super) fn placement_rect(rect: (i32, i32, i32, i32), scale: f32) -> OverlayPlacement {
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
    // mimalloc 的缓存页默认留着不还 OS;候选窗收起即输入间歇,
    // 趁空闲把本线程堆的空闲段还给系统,压住常驻内存水位。
    unsafe { libmimalloc_sys::mi_collect(true) };
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
    s.backdrop_read = None;
    s.backdrop_raw = None;
    s.grain = None;
    s.grain_size = None;
    s.glaze = None;
    s.glaze_read = None;
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

/// 抓取面板矩形外扩折射带宽的桌面像素,过液体玻璃光学后作为玻璃背板;
/// 每帧重捕,背景变动跟随。面板外的空带不铺像素,直接透出真桌面。
fn capture_backdrop(s: &mut Candidate) -> bool {
    let (x, y, w, h) = s.fx.placement.panel;
    if w <= 0 || h <= 0 {
        return false;
    }
    let scale = s.scale;
    let pad = liquid::capture_pad(s.glass_band, s.glass.pull, scale);
    let Some((raw_w, raw_h, raw)) =
        capture_rect(x - pad, y - pad, (w + 2 * pad) as u32, (h + 2 * pad) as u32)
    else {
        return false;
    };
    // 原图没变(窗口没动、桌面静止)就不重算光学、不重新上传。
    if s.backdrop_raw
        .as_ref()
        .is_some_and(|(size, previous)| *size == (raw_w, raw_h) && **previous == raw)
    {
        return false;
    }
    let radius = panel::PANEL_RADIUS * scale;
    let optics = liquid::process_with_pull(
        &raw,
        raw_w,
        raw_h,
        pad,
        w as u32,
        h as u32,
        s.glass_band * scale,
        radius,
        s.glass.pull,
    );
    let changed = s
        .backdrop
        .get_or_insert_with(Backdrop::default)
        .publish(w as u32, h as u32, optics);
    s.backdrop_raw = Some(((raw_w, raw_h), raw));
    let size = (w as u32, h as u32);
    if s.grain_size != Some(size) {
        let pixels = Pixels::new();
        pixels.set(w as u32, h as u32, liquid::grain_rgba(w as u32, h as u32));
        s.grain = Some(pixels);
        s.grain_size = Some(size);
    }
    changed
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
    let now = s.fx.now;
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
fn view(s: &Candidate, cx: &mut rune::Cx<Candidate>) -> El {
    // 系统关掉"动画效果"时,框架把弹簧动效降级为淡出;上游宿主在
    // 建窗与焦点变化时读同一个 SPI,直接取它喂下的值。
    cx.reduced_motion(cx.system_reduced_motion());
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
    let glass = !s.private && s.material.is_glass() && s.glass_wanted(high_contrast);
    // 高透到磨砂的连续插值:压光与模糊同步加重;端点由变体给出。
    let frost = s.frost.clamp(0.0, 1.0);
    let frost_blur = s.glass.base_blur + (FROST_BLUR - s.glass.base_blur) * frost;
    let frost_fill = rgba(
        0x262624,
        s.glass.base_tint + (0.62 - s.glass.base_tint) * frost,
    );
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
    if glass && let Some(backdrop) = &s.backdrop {
        // 活背板只铺面板矩形:它只给玻璃的 backdrop_blur 当底图。
        // 面板外的空带不画任何东西,alpha=0 直接透出真桌面——铺整张
        // 捕获图反而会把错位的残影留在候选页即将展开的位置。
        // transition/motion 与 body 同款:展开/收起与入场时跟面板走同一条
        // 轨迹,否则方角补丁会在圆角面板外闪一下。
        // 背板贴屏不贴形:截图按最终面板矩形一次抓成,展开/收起时
        // 元素矩形仍跟 body 同一条弹簧,但像素 1:1 不缩放(真玻璃是
        // 光圈在屏上滑,折射图样不跟着拉伸)。锚边取贴光标的一侧:
        // 向上展开钉底、向下展开钉顶,收起时正好停在固定条的位置。
        root = root.child(
            el().absolute(bx, by)
                .w(s.fx.placement.panel.2 as f32 / scale)
                .h(s.fx.placement.panel.3 as f32 / scale)
                .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
                .motion(motion::SETTLE)
                // 方形图像补丁垫在圆角面板下,角上会戳出来——同半径裁圆。
                .clip_rounded(panel::PANEL_RADIUS * scale)
                .pixels_fit(&backdrop.pixels, SurfaceFit::Fill)
                .surface_unscaled(true)
                .surface_anchor(
                    Align::Start,
                    if s.fx.above == Some(true) {
                        Align::End
                    } else {
                        Align::Start
                    },
                ),
        );
    }
    let mut body = col()
        .absolute(bx, by)
        .w(s.fx.placement.panel.2 as f32 / scale)
        .h(s.fx.placement.panel.3 as f32 / scale)
        // key 锚定 key 路径:sel/格子的 keyed 祖先两侧一致,条↔棋盘
        // 是同一节点——sel 才能持久化做矩形渐变拉伸。
        .key("body")
        .hit()
        // 入场:从光标侧滑入并淡入;挂载过渡只在 animated 提交里播,
        // fresh 路径显式打 tx.animated()。顺带盖住复显首帧的透明空窗。
        .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
        .motion(motion::SETTLE)
        .radius(panel::PANEL_RADIUS * scale)
        .continuous_corners()
        .clip_rounded(panel::PANEL_RADIUS * scale)
        .on_wheel(|s: &mut Candidate, e, _| s.on_wheel_points(e.dy))
        .on_secondary_press_local(|s: &mut Candidate, p, _| s.on_secondary(p.x, p.y));
    match (s.material, glass) {
        // 玻璃:探针配方的连续插值,滑杆从高透滑到磨砂。背板是过液体
        // 玻璃光学的桌面像素(棱缘折射 + 色散 + 提饱和),模糊仍由
        // backdrop_blur 做。
        (UiMaterial::Glass, true) => {
            body = body
                .backdrop_blur(frost_blur)
                .fill(frost_fill)
                .gradient(frost_top, frost_bottom)
                .border(1.0 * scale, rgba(0xffffff, s.glass.border));
            // 加深的 meniscus 内阴影(变体 C/D):贴底的静观玻璃感。
            // pin 底边:面板形变期间跟着动画中的底缘走,absolute 会钉在
            // 终态高度上提前脱底。
            if s.glass.meniscus {
                body = body.child(
                    el().pin(Some(0.0), None, Some(0.0), Some(0.0))
                        .h(2.0 * scale)
                        .fill(rgba(0x000000, 0.28)),
                );
            }
            // 亚克力颗粒:固定噪点图垫在模糊之上,blur 抹不掉。与背板
            // 同一条纪律:噪点 1:1 不随面板形变拉伸,钉在贴光标的一侧。
            if let Some(grain) = &s.grain {
                body = body.child(
                    el().absolute(0.0, 0.0)
                        .w_full()
                        .h_full()
                        .pixels_fit(grain, SurfaceFit::Fill)
                        .surface_unscaled(true)
                        .surface_anchor(
                            Align::Start,
                            if s.fx.above == Some(true) {
                                Align::End
                            } else {
                                Align::Start
                            },
                        ),
                );
            }
            // 环境感知的棱缘 rim:上下 1px 釉线随环境光亮暗,玻璃的
            // Fresnel 边光近似;「光必有来源」与瓷釉同一套纪律。下缘
            // pin 底边,形变期间跟着动画中的底缘走。
            if let Some(glaze) = &s.glaze {
                body = body
                    .child(
                        el().pin(Some(0.0), Some(0.0), Some(0.0), None)
                            .h(1.0 * scale)
                            .fill(glaze.edge_top),
                    )
                    .child(
                        el().pin(Some(0.0), None, Some(0.0), Some(0.0))
                            .h(1.0 * scale)
                            .fill(glaze.edge_bottom),
                    );
            }
        }
        // 黑瓷:不透明暖黑胎,同构釉面。釉面的光全部来自环境采样:
        // 面板上方亮则光泽与上缘亮,环境暗则哑光,没有凭空的光。
        (UiMaterial::Ceramic, _) if !high_contrast => {
            body = body
                .gradient(rgb(0x363029), rgb(0x28241d))
                .border(1.0 * scale, rgb(palette.border));
            if let Some(glaze) = &s.glaze {
                body = body
                    .child(
                        el().pin(Some(0.0), Some(0.0), Some(0.0), None)
                            .h((s.fx.placement.panel.3 as f32 / scale) * 0.45)
                            .gradient(glaze.sheen_top, glaze.sheen_top.alpha(0.0)),
                    )
                    .child(
                        el().pin(Some(0.0), Some(0.0), Some(0.0), None)
                            .h(1.0 * scale)
                            .fill(glaze.edge_top),
                    )
                    .child(
                        el().pin(Some(0.0), None, Some(0.0), Some(0.0))
                            .h(1.0 * scale)
                            .fill(glaze.edge_bottom),
                    );
            }
        }
        // 背板/动效不可用:实底主题兜底,绝不露原始桌面。
        _ => {
            body = body
                .fill(rgb(palette.bg))
                .border(1.0 * scale, rgb(palette.border));
        }
    }
    let glow = lueying_server::theme::glow_of(palette.accent);
    let selected_ink = if high_contrast {
        rgb(palette.selected_text)
    } else {
        glow.ink
    };
    let look = Look {
        palette: &palette,
        glow,
        selected_ink,
        high_contrast,
    };
    body = body.child(content(s, geom, &look));
    root = root.child(body);
    if let Menu::Open { gidx, text, .. } = &s.fx.menu
        && s.fx
            .last
            .as_ref()
            .is_some_and(|last| last.view.candidates.get(*gidx) == Some(text))
        && menu_rect(*gidx, s, geom).is_some()
    {
        root = root.child(menu_card(s, geom, &look, *gidx, text));
    }
    root
}

/// 选中槽的目标矩形(面板本地逻辑点);place() 与 content() 共用一份
/// 计算——液桥的起点/终点与它严格同源。
fn sel_rect_of(s: &Candidate, geom: &Geom) -> Option<Rect> {
    let meas: &dyn Measure = s;
    if geom.spec.expand && matches!(geom.spec.layout, Layout::Wrap) {
        let cells = panel::grid_cells(&geom.spec, &geom.metrics, meas);
        geom.spec
            .grid
            .iter()
            .zip(&cells)
            .find(|(c, _)| c.selected)
            .map(|(_, r)| *r)
    } else {
        let rects = panel::item_rects(&geom.spec, &geom.metrics, meas);
        geom.spec
            .items
            .iter()
            .zip(&rects)
            .find(|(i, _)| i.selected)
            .map(|(_, r)| *r)
    }
}

/// 面板内容:固定条或展开棋盘,加右侧控制轨与条尾箭头。
fn content(s: &Candidate, geom: &Geom, look: &Look) -> El {
    let meas: &dyn Measure = s;
    // 条与棋盘共用同一层视口:格子与字的父路径两侧一致,key 才能跨翻转
    // 持久(父节点一变就走重挂载,滑行动画全断;当年 sel 同理)。字的
    // 滑行=持久节点的布局弹簧,天然从当前视觉位置出发,「哪来回哪去」
    // 由构造保证,不受翻转打断影响。
    let expanded = geom.spec.expand && matches!(geom.spec.layout, Layout::Wrap);
    let scale = geom.metrics.scale();
    let sel_rect = sel_rect_of(s, geom);
    // 视口与控制层都是 absolute(0,0) 覆盖:el() 的子节点纵向排流,
    // 流内的第二个全尺寸子节点会被推到面板外——控制轨(箭头/滚动条)
    // 消失就是这个原因;全部改覆盖层,不留流内兄弟。
    let mut viewport = el().absolute(0.0, 0.0).w_full().h_full();
    if expanded {
        // 棋盘:上下边缘线性渐隐(clip_fade_linear 的文档场景:
        // "a transcript fading into the top of a composer"),提示还有内容没滚到。
        viewport = viewport.clip_fade_linear(0.0, 16.0 * scale, 0.0, 16.0 * scale);
    }
    // 翻页推入挂在视口上:只在翻页滑动时播;挂在条自己的包装层会断开
    // 格子两侧的父路径。fresh 的 animated 提交里挂载不重演,否则会与
    // 面板入场过渡叠着闪。key 钉住视口身份:液块水珠/选中标记在它
    // 前面来去,没 key 时它的父内 index 跟着挪=每帧重挂载,武装过
    // 的推入被反复回放,元素看着从右缘一次次飞入。
    viewport = viewport.key("viewport");
    viewport = viewport.transition(s.fx.slide_edge.map_or(Transition::none(), |edge| {
        // 推入叠加官方 blur_replace(整块模糊替换 + 轻缩放),翻页更软。
        Transition::push(edge).combined(Transition::blur_replace())
    }));
    if expanded {
        // 棋盘:所有候选一样大,文字居中;控制轨收在最右。
        let cells = panel::grid_cells(&geom.spec, &geom.metrics, meas);
        for (candidate, rect) in geom.spec.grid.iter().zip(&cells) {
            viewport = viewport.child(cell_el(
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
        // 固定条:自然宽度候选从左排到右。
        let rects = panel::item_rects(&geom.spec, &geom.metrics, meas);
        let page_start = s.page_start();
        for (index, (item, rect)) in geom.spec.items.iter().zip(&rects).enumerate() {
            viewport = viewport.child(item_el(
                s,
                geom,
                look,
                item,
                &Cell {
                    rect: *rect,
                    index,
                    gidx: page_start + index,
                    hovered: s.fx.hover == Some(index),
                },
            ));
        }
    }
    // sel 垫在格子下,两侧都挂 layer(父路径一致才持久);先于视口画。
    let mut layer = el().w_full().h_full();
    if let Some(target) = sel_rect {
        // 选中几何:原生 lead 驱动前/后缘弹簧,重定向保速度;这里只
        // 读它每帧写回的绘制矩形,重建水滴轮廓。几何本身透明(零
        // alpha 仍会被画,`drawn_rect_into` 每帧有值),轮廓盖在它上面。
        let scale = geom.metrics.scale();
        let (panel_x, panel_y) = s.fx.sel_origin;
        let live = s.sel_drawn.get();
        let drawn = if s.fx.sel_snap || live[2] <= 0.0 {
            target
        } else {
            Rect::from_xywh(live[0] - panel_x, live[1] - panel_y, live[2], live[3])
        };
        let mut geom_el = el()
            .key("sel-geom")
            .absolute(target.left, target.top)
            .w(target.width())
            .h(target.height())
            .radius(target.height() * 0.5)
            .fill(Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            })
            .lead(motion::LEAD, motion::DEFAULT)
            .drawn_rect_into(&s.sel_drawn);
        if s.fx.sel_snap || !s.effects() || s.fx.reduce || s.fx.scroll_drag.is_some() {
            geom_el = geom_el.instant_changes();
        }
        layer = layer.child(geom_el);

        let axis_x = s.fx.sel_axis_x;
        let fwd = if s.fx.sel_forward < 0.0 { -1.0 } else { 1.0 };
        let (main_len, head_d) = if axis_x {
            (drawn.width(), drawn.height())
        } else {
            (drawn.height(), drawn.width())
        };
        let rest_main = if axis_x {
            target.width()
        } else {
            target.height()
        };
        let r_cap = target.width().min(target.height()) / 2.0;
        let extra = (main_len - rest_main).max(0.0);
        let cap = r_cap * GOO_CAP;
        let shown_main = main_len.min(rest_main + cap);
        let shown = if axis_x {
            let head = if fwd > 0.0 { drawn.right } else { drawn.left };
            let left = if fwd > 0.0 { head - shown_main } else { head };
            Rect::from_xywh(left, drawn.top, shown_main, drawn.height())
        } else {
            let head = if fwd > 0.0 { drawn.bottom } else { drawn.top };
            let top = if fwd > 0.0 { head - shown_main } else { head };
            Rect::from_xywh(drawn.left, top, drawn.width(), shown_main)
        };
        // 几何 sanity:原生弹簧只会在面板内动,这里仍留一道保险——
        // 非有限/出板不画,绝不把旧形态坐标画成整盘蓝块。
        let (pw, ph) = (geom.width, geom.height);
        let bad = !(shown.left.is_finite()
            && shown.top.is_finite()
            && shown.width().is_finite()
            && shown.height().is_finite())
            || shown.left < 0.0
            || shown.top < 0.0
            || shown.right > pw
            || shown.bottom > ph;
        if bad {
            eprintln!(
                "sel_goo 几何越界 left={:.0} top={:.0} w={:.0} h={:.0} \
                 pw={pw:.0} ph={ph:.0} main={main_len:.0} rest={rest_main:.0} \
                 axis_x={axis_x}",
                shown.left,
                shown.top,
                shown.width(),
                shown.height()
            );
        } else {
            // 拉得够长就是水滴:单条轮廓,滞后缘收尖、领先端整圆。
            // 短拉伸保持胶囊,尖头不露怯。
            if extra > head_d * GOO_DROP_RATIO && shown_main > head_d * 1.5 {
                layer = layer.child(goo_drop_marker(
                    shown_main, head_d, axis_x, fwd, look, scale, shown,
                ));
            } else {
                layer = layer.child(sel_marker(
                    wobbled(s.fx.sel_wobble, s.fx.now, shown),
                    look,
                    scale,
                ));
            }
            // 甩脱的水滴:原生尾长超过封顶的部分截掉,只剩快连击时
            // 在封顶外撒几颗同灯珠。flat 纯色点真机读作噪点,灯珠
            // (渐变+小光晕)才读作液体。
            let shed = main_len - shown_main;
            if shed > r_cap * 0.8 && s.fx.sel_peak_v > GOO_SHED_V {
                let reach = shed.min(r_cap * 8.0);
                let n_drop = ((reach / (r_cap * 3.0)).floor() as usize).clamp(2, 4);
                let n_c = shown.top + shown.height() * 0.5;
                let n_cx = shown.left + shown.width() * 0.5;
                let head_u = if axis_x {
                    if fwd > 0.0 { shown.right } else { shown.left }
                } else if fwd > 0.0 {
                    shown.bottom
                } else {
                    shown.top
                };
                for k in 1..=n_drop {
                    // 0 贴封顶缘(大而实),1 远端(小而淡)。
                    let f = (k - 1) as f32 / (n_drop.max(2) - 1) as f32;
                    let u = shown_main + reach * f;
                    let (dx2, dy2) = if axis_x {
                        (head_u - fwd * u, n_c)
                    } else {
                        (n_cx, head_u - fwd * u)
                    };
                    let d_r = (r_cap * (0.42 + 0.36 * (1.0 - f))).min(20.0);
                    let d_a = 0.45 + 0.50 * (1.0 - f);
                    if !(dx2.is_finite() && dy2.is_finite())
                        || dx2 - d_r < 0.0
                        || dy2 - d_r < 0.0
                        || dx2 + d_r > geom.width
                        || dy2 + d_r > geom.height
                    {
                        continue;
                    }
                    let mut bead = el()
                        .absolute(dx2 - d_r, dy2 - d_r)
                        .w(d_r * 2.0)
                        .h(d_r * 2.0)
                        .radius(d_r)
                        .opacity(d_a);
                    bead = if look.high_contrast {
                        bead.fill(rgb(look.palette.selected_bg))
                    } else {
                        bead.gradient(look.glow.core_top, look.glow.core_bottom)
                            .shadow(
                                0.0,
                                d_r * 0.9 * scale,
                                look.glow.core_bottom.alpha(0.30 * d_a),
                            )
                    };
                    layer = layer.child(bead);
                }
            }
        }
    }
    layer = layer.child(viewport);
    layer = layer.child(control_tree(s, geom, look));
    layer
}

/// 聚合余震:液块归位后衰减正弦(水滴落地)。高度抖动、宽度反向
/// 补偿(体积守恒的果冻),中心锚定。约 0.5s 三个半波衰减看不见。
fn wobbled(wobble: Option<(f64, f32)>, now: f64, r: Rect) -> Rect {
    let Some((w0, amp)) = wobble else {
        return r;
    };
    let t = (now - w0) as f32;
    let w = amp * (-9.0 * t).exp() * (core::f32::consts::TAU * 6.0 * t).sin();
    if w.abs() < 0.003 && t > 0.4 {
        return r;
    }
    let dh = r.height() * w;
    let dw = -r.width() * w * 0.55;
    Rect::from_xywh(
        r.left - dw / 2.0,
        r.top - dh / 2.0,
        r.width() + dw,
        r.height() + dh,
    )
}

/// 液滴轮廓:滞后缘(u=0)一个尖,两侧直线切到领先端直径 H 的整圆
/// 上(切点在圆的左上/左下弧段,不在顶点),圆取两切点间的大弧——
/// 头是一颗完整的球,尾是一条锥,数学上就是彗星/水滴剪影。侧边用
/// quadratic 向外微鼓(0.06H)去机械感。坐标在归一化 (u,n):u 主轴
/// 0→W 指领先端,n 侧轴;axis_y 转置,fwd<0 沿主轴镜像。
/// 离线光栅化调过的参数,改之前先用 /tmp/drop.py 看剪影。
fn drop_path(w: f32, h: f32, map: &dyn Fn(f32, f32) -> [f32; 2]) -> Path {
    let r = h * 0.5;
    // 圆心最多顶到 u=W-r;W 太小时 clamp 防 acos 越域。
    let cx = (w - r).max(r * 1.2);
    let beta = (-r / cx).acos();
    // 切点相对圆心:(cos β, ±sin β),cos β = -r/cx 是负的→切点在左半圆。
    let up = (cx + r * beta.cos(), r - r * beta.sin());
    let down = (cx + r * beta.cos(), r + r * beta.sin());
    let bow = h * 0.06;
    let mut path = Path::new()
        .move_to(map(0.0, r))
        .quad_to(map(up.0 * 0.5, (r + up.1) * 0.5 - bow), map(up.0, up.1));
    // 头圆大弧:-β → +β 经过 0°(正右),约 200° 采样 20 段。
    for i in 1..=20 {
        let a = -beta + 2.0 * beta * (i as f32 / 20.0);
        path = path.line_to(map(cx + r * a.cos(), r + r * a.sin()));
    }
    path.quad_to(map(down.0 * 0.5, (r + down.1) * 0.5 + bow), map(0.0, r))
        .close()
}

/// 拉伸态选中液滴:单个元素、单条轮廓,与静止胶囊同一盏灯。
/// 渐变拆成「上亮下暗」:主体 core_bottom,上半部压一条同色轮廓的
/// 高光填充;外圈同轮廓描边晕替胶囊的投影(shape 不吃元素阴影)。
fn goo_drop_marker(
    w: f32,
    h: f32,
    axis_x: bool,
    fwd: f32,
    look: &Look,
    scale: f32,
    rect: Rect,
) -> El {
    let m = |u: f32, n: f32| -> [f32; 2] {
        let u = if fwd < 0.0 { w - u } else { u };
        if axis_x { [u, n] } else { [n, u] }
    };
    let el = el()
        .absolute(rect.left, rect.top)
        .w(rect.width())
        .h(rect.height())
        .key("sel");
    if look.high_contrast {
        return el
            .shape(Shape::new().fill_path(&drop_path(w, h, &m), rgb(look.palette.selected_bg)));
    }
    let body = drop_path(w, h, &m);
    // 高光:同轮廓压到上 55% 高度,底部弦自然闭合,贴合上缘不留缝。
    let sheen = drop_path(w, h * 0.55, &m);
    el.shape(
        Shape::new()
            .stroke_path(
                &body,
                7.0 * scale,
                LineCap::Round,
                look.glow.core_bottom.alpha(0.16),
            )
            .fill_path(&body, look.glow.core_bottom)
            .fill_path(&sheen, look.glow.core_top.alpha(0.80)),
    )
}

/// 选中指示器:单一共享元素垫在选中槽下,而不是每格各自开关装饰子树。
/// 选中迁移的几何由 content 里的 `sel-geom` 原生 lead 驱动;这里只按
/// 它写出的绘制矩形画胶囊/水滴,元素自己不再配 `.motion`。
fn sel_marker(rect: Rect, look: &Look, scale: f32) -> El {
    selection_pill(
        el().absolute(rect.left, rect.top)
            .w(rect.width())
            .h(rect.height())
            .radius(rect.height() * 0.5),
        look,
        scale,
    )
    // key+持久身份:条↔棋盘同一节点。
    // 不用 matched:胶囊飞行走 launches 通道,会盖住同路径 top1
    // 的字("猛沉")——持久形变照常拉伸,且永远画在飞行层下面。
    // 不配 transition:重挂载时的淡入淡出会顶掉形变,看着像胶囊
    // 消失重冒而不是拉伸。
    .key("sel")
}

/// 选中槽:发光胶囊。它自己是面板的光源:灯芯上白热下饱和,双层光晕
/// 落在胎上——玻璃读作磨砂下的透光,瓷读作照亮黑瓷。灯色跟主题色走。
/// 高对比度回归系统选中色的平涂:光源是装饰语义。
fn selection_pill(cell: El, look: &Look, scale: f32) -> El {
    if look.high_contrast {
        return cell.fill(rgb(look.palette.selected_bg));
    }
    cell.gradient(look.glow.core_top, look.glow.core_bottom)
        .shadow(0.0, 14.0 * scale, look.glow.core_bottom.alpha(0.30))
        .shadow(0.0, 26.0 * scale, look.glow.core_bottom.alpha(0.16))
}

/// 一个候选格的布局事实:矩形、页内序号、全列表序号与悬停态。
#[derive(Clone, Copy)]
struct Cell {
    rect: Rect,
    index: usize,
    gidx: usize,
    hovered: bool,
}

/// 固定条的一条候选:序号、文字与注音;选中垫发光胶囊,灯色跟主题色。
fn item_el(s: &Candidate, geom: &Geom, look: &Look, item: &Item, at: &Cell) -> El {
    let Cell {
        rect,
        index,
        gidx,
        hovered,
    } = *at;
    let scale = geom.metrics.scale();
    let mut cell = col()
        .absolute(rect.left, rect.top)
        .w(rect.width())
        .h(rect.height())
        .radius(rect.height() * 0.5)
        .cursor(Cursor::Pointer)
        .role("button")
        .label(item.text.clone())
        // key 持久身份:无 key 的格子 id 会被回收,撞上退出中节点的
        // id 就继承它在途的动画位置(日志实测 486)——整个格子带字从
        // 右边滑回来。同 key 让条↔棋盘是同一节点。不配 motion:格
        // 子矩形瞬间到位,字的飞行落点不中途挪动,路径保持直线。
        .key(format!("cell-{}-{gidx}-{}", s.comp, item.text))
        .press_on_down()
        .on_press(move |s: &mut Candidate| s.pick_slot(index));
    // 格子本身透明不可见,不需要飞;真正"哪来回哪去"的是字。
    // 悬停高亮:原生路径交给 El::hover 悬停叠加(零视图提交);
    // 回落注入路径没有元素悬停,仍读 fx.hover 画 fill。
    if !item.selected {
        cell = if hovered {
            cell.fill(look.tint(look.palette.hover, 0x66))
        } else {
            cell.hover(look.tint(look.palette.hover, 0x66))
        };
    }
    let index_ink = if item.selected {
        // 亮芯上的数字与选中文字同一墨:灯色的深色同源(光越亮字越深)。
        // accent_text 是深底上的浅色主题字(如 0x7aa7ff),落在浅色灯芯
        // 上根本看不清。
        look.selected_ink
    } else {
        rgb(look.palette.text_tertiary)
    };
    cell = cell
        .child(
            text(format!("{}", index + 1))
                .absolute(
                    panel::ITEM_PAD * scale,
                    ink_y(rect.height(), geom.metrics.small),
                )
                .w(20.0 * scale)
                .font(TextStyle::new(geom.metrics.small, 400.0))
                .color(index_ink)
                .a11y_hidden(),
        )
        .child(
            // 字的持久节点:条与棋盘两侧同一个 key,翻转时节点不重挂,
            // 布局弹簧原生把它从当前视觉位置滑到新位置——「哪来回哪去」
            // 由构造保证。曾用两侧不同 key + El::matched 配对飞行,实测
            // 两种翻车:matched 起点读旧节点的布局矩形、无视它在途的
            // travel 偏移,飞行到一半再翻转就跳回旧布局位置重飞;翻转
            // 几次后 NodeId 回收的场景残留又让配对起点失真。
            text(ellipsized(s, geom, &item.text))
                .absolute(
                    panel::TEXT_X * scale,
                    ink_y(rect.height(), geom.metrics.text),
                )
                .key(format!("word-{}-{gidx}-{}", s.comp, item.text))
                .font(TextStyle::new(geom.metrics.text, 400.0))
                .color(if item.selected {
                    look.selected_ink
                } else {
                    rgb(look.palette.text)
                })
                .a11y_hidden(),
        );
    if !geom.spec.is_strip() && !item.hint.is_empty() {
        let shown = ellipsized(s, geom, &item.text);
        let hint_x = panel::TEXT_X * scale + s.text_width(&shown, geom.metrics.text) + 4.0 * scale;
        cell = cell.child(
            text(item.hint.clone())
                .absolute(hint_x, ink_y(rect.height(), geom.metrics.small))
                .w(100.0 * scale)
                .font(TextStyle::new(geom.metrics.small, 400.0))
                .color(rgb(look.palette.text_tertiary)),
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
    // absolute 覆盖层:流内的全尺寸层会被前面的视口挤到面板外。
    let mut layer = el().absolute(0.0, 0.0).w_full().h_full().key("controls");
    let rail = panel::rail_rect(&geom.spec, &geom.metrics, meas, geom.height);
    let cap = panel::chevron_rect(&geom.spec, &geom.metrics, meas);
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
            .child({
                // 键控滑块:位置交元素布局动画。行进时后缘欠阻尼,撞端
                // 过冲就是压缩、回弹复原;帧由 rt.animating 保,不押在
                // 会被宿主滤掉的定时器唤醒上。拖动与减动态即时跟手。
                let mut el = el()
                    .key("grid-thumb")
                    .absolute(thumb.left, thumb.top)
                    .w(thumb.width())
                    .h(thumb.height())
                    .radius(thumb.width() * 0.5)
                    .fill(look.tint(look.palette.text_tertiary, thumb_a))
                    .motion(THUMB_LEAD)
                    .lead(THUMB_LEAD, THUMB_TRAIL);
                if s.fx.reduce || s.fx.scroll_drag.is_some() {
                    el = el.instant_changes();
                }
                el
            });
    }
    if let Some(cap) = cap {
        let expanded = geom.spec.expand && matches!(geom.spec.layout, Layout::Wrap);
        // 箭头和 rail 是同一元素:展开+可滚时它的矩形就是整条 rail,
        // 持久节点 motion 把按钮弹着拉伸成轨道。
        let track = rail.unwrap_or(cap);
        let is_rail = expanded && rail.is_some();
        let mut chev = el()
            .absolute(track.left, track.top)
            .w(track.width())
            .h(track.height())
            .cursor(Cursor::Pointer)
            .role("button")
            .label_static(if expanded {
                "收起候选窗"
            } else {
                "展开候选窗"
            })
            // 同一 key 持久节点:rect 从按钮形变到 rail 走 motion;
            // 无 transition 不淡入淡出。慢弹簧:前半段被生长的 body
            // 裁剪边盖住,后半段在面板定型后继续可见地拉伸——按钮
            // 延迟着长成滚动条。
            .key("chev")
            .motion(Spring::new(0.5, 0.35))
            .transition(Transition::none())
            .on_hover_change(|s: &mut Candidate, inside, _| {
                s.fx.chev = inside;
                s.fx.scroll_hover = inside;
            })
            // 命中顶帽=收起/展开,命中轨道=拖滚;Down 即判定。
            // 正常会话真实输入直达元素,不走 fallback。
            .on_drag(move |s: &mut Candidate, drag, _| match drag.phase {
                rune::Phase::Down => {
                    let [x, y] = drag.window_point();
                    if x >= cap.left
                        && x <= cap.left + cap.width()
                        && y >= cap.top
                        && y <= cap.top + cap.height()
                    {
                        s.expand_toggle();
                    } else {
                        s.scroll_grab_at(x, y);
                    }
                }
                rune::Phase::Move => {
                    let [_, y] = drag.window_point();
                    s.scroll_drag_to(y);
                }
                rune::Phase::Up => s.fx.scroll_drag = None,
            });
        if is_rail {
            chev = chev
                .radius(track.width() * 0.5)
                .fill(tint(0x000000, 0x34))
                .border(1.0 * scale, tint(0xffffff, 0x14));
        } else {
            chev = chev.radius(track.height() * 0.5);
            let alpha = match (expanded, s.fx.chev) {
                (true, true) => 0x66,
                (true, false) => 0x38,
                (false, true) => 0x4d,
                (false, false) => 0x00,
            };
            if alpha > 0 {
                chev = chev.fill(look.tint(look.palette.border, alpha));
            }
        }
        // 箭头折线钉在顶帽(cap)内,再换算进元素(track)本地坐标。
        let turn = if geom.spec.expand {
            std::f32::consts::PI
        } else {
            0.0
        };
        let [a, b, c] = panel::chevron_points_turn(cap, turn, scale);
        let (a, b, c) = (
            (a.x - track.left, a.y - track.top),
            (b.x - track.left, b.y - track.top),
            (c.x - track.left, c.y - track.top),
        );
        let (color, width) = (look.tint(look.palette.text_secondary, 0xff), 1.6 * scale);
        // 折线画成 canvas 子元素:交互元素自己挂 shape 不渲染(探针对照
        // 实锤,非交互才画);canvas 子节点不受影响,坐标同是 track 本地。
        chev = chev.child(
            canvas(chevron_segment(
                chevron_segment(Shape::new(), a, b, width, color),
                b,
                c,
                width,
                color,
            ))
            .w(track.width())
            .h(track.height()),
        );
        layer = layer.child(chev);
    }
    layer
}

/// Preserve the former rotated capsule's outer endpoints with the vector stroke's round caps.
fn chevron_segment(shape: Shape, a: (f32, f32), b: (f32, f32), width: f32, color: Color) -> Shape {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let t = (width * 0.5 / len).min(0.49);
    shape.line(
        [a.0 + dx * t, a.1 + dy * t],
        [b.0 - dx * t, b.1 - dy * t],
        width,
        LineCap::Round,
        color,
    )
}

/// 右键菜单:固定形状,外层 10px、行内缩 4px;玻璃上也用实底。
/// 原生路径:锚在候选格子的 key 上,放不下自动翻到行上、8pt 窗口内
/// 钳制(El::anchored);modal 提供外点关闭,行悬停/键盘高亮同享
/// focus_highlight(NSMenu 同款)。回落注入的路径不走元素树,悬停
/// 仍由 fx.menu.hover 画 fill、命中走 menu_rect + panel::menu_hit。
fn menu_card(s: &Candidate, geom: &Geom, look: &Look, gidx: usize, text_: &str) -> El {
    let scale = geom.metrics.scale();
    let w = 116.0 * scale;
    let h = panel::menu_height(&geom.metrics);
    let tint = rgb(look.palette.hover);
    let mut card = col()
        .anchored(
            format!("cell-{}-{gidx}-{text_}", s.comp),
            Side::Bottom,
            Align::Start,
            0.0,
        )
        .anchored_round()
        .hit()
        .w(w)
        .h(h)
        .radius(10.0 * scale)
        .fill(rgb(look.palette.bg))
        .border(1.0 * scale, rgb(look.palette.border))
        .role("menu")
        .focus_group()
        .modal(true, |s: &mut Candidate| s.fx.menu = Menu::Closed);
    for (index, label) in panel::MENU_ITEMS.iter().enumerate() {
        let y = (4.0 + index as f32 * 28.0) * scale;
        let hovered = s.fx.menu.hover() == Some(index);
        let mut row = el()
            .absolute(4.0 * scale, y)
            .w((w - 8.0 * scale).max(0.0))
            .h(26.0 * scale)
            .radius(panel::concentric_radius(10.0 * scale, 4.0 * scale, 0.0))
            .cursor(Cursor::Pointer)
            .role("menuitem")
            .label_static(label)
            .focusable(true)
            .focus_highlight(tint)
            .press_on_down()
            .on_press(move |s: &mut Candidate| s.menu_click(index));
        if hovered {
            row = row.fill(tint);
        }
        card = card.child(
            row.child(
                text(*label)
                    .absolute(0.0, ink_y(26.0 * scale, 12.0 * scale))
                    .font(TextStyle::new(12.0 * scale, 400.0))
                    .color(rgb(if hovered {
                        look.palette.text
                    } else {
                        look.palette.text_secondary
                    }))
                    .a11y_hidden(),
            ),
        );
    }
    card
}

/// 菜单矩形(面板本地逻辑点):与 El::anchored 同一份数学(anchor_rect),
/// 回落注入路径拿它做命中与悬停;锚格不在本页/棋盘时返回 None,
/// 视图不挂菜单(anchored 解析不到锚也会压到零矩形,双保险)。
fn menu_rect(gidx: usize, s: &Candidate, geom: &Geom) -> Option<Rect> {
    let scale = geom.metrics.scale();
    let meas: &dyn Measure = s;
    let expanded = geom.spec.expand && matches!(geom.spec.layout, Layout::Wrap);
    let cells = if expanded {
        panel::grid_cells(&geom.spec, &geom.metrics, meas)
    } else {
        panel::item_rects(&geom.spec, &geom.metrics, meas)
    };
    let cell = if expanded {
        geom.spec
            .grid
            .iter()
            .zip(&cells)
            .find(|(cell, _)| cell.index == gidx)
            .map(|(_, rect)| *rect)?
    } else {
        cells.get(gidx.checked_sub(s.page_start())?).copied()?
    };
    // 面板本地 → 窗口本地(与 body 的 absolute(bx, by) 同一步换算)。
    let bx = (s.fx.placement.panel.0 - s.fx.placement.window.0) as f32 / scale;
    let by = (s.fx.placement.panel.1 - s.fx.placement.window.1) as f32 / scale;
    let target = [
        cell.left + bx,
        cell.top + by,
        cell.right + bx,
        cell.bottom + by,
    ];
    let win = (
        s.fx.placement.window.2 as f32 / scale,
        s.fx.placement.window.3 as f32 / scale,
    );
    let r = anchor_rect(
        target,
        (116.0 * scale, panel::menu_height(&geom.metrics)),
        Side::Bottom,
        Align::Start,
        0.0,
        win,
    );
    Some(Rect::from_ltrb(r[0] - bx, r[1] - by, r[2] - bx, r[3] - by))
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

    /// 菜单项:全局候选序号取自菜单状态,展开棋盘里的页外行也能置顶/降权。
    fn menu_click(&mut self, item: usize) {
        if !self.interactive() {
            return;
        }
        let Menu::Open { gidx, .. } = self.fx.menu else {
            return;
        };
        let session = self.state.active.load(Ordering::Relaxed);
        let req = if item == 0 {
            Request::PinIndex { session, gidx }
        } else {
            Request::DemoteIndex { session, gidx }
        };
        self.request(req);
        self.fx.menu = Menu::Closed;
    }

    fn scroll_to(&mut self, offset: usize) {
        let session = self.state.active.load(Ordering::Relaxed);
        let _ = imp::handle(&self.state, Request::ScrollTo { session, offset });
    }

    /// 滚轮一档:固定条上滚轮展开棋盘,棋盘里滚轮翻整页。返回是否真的
    /// 消费了这一档:不可交互时让出,事件穿透给宿主。
    fn wheel_step(&mut self, step: i32) -> bool {
        if !self.interactive() {
            return false;
        }
        let session = self.state.active.load(Ordering::Relaxed);
        let Some(published) = self.state.active_published() else {
            return false;
        };
        let (expanded, len) = (published.view.expanded, published.view.candidates.len());
        if len <= 1 {
            return false;
        }
        if expanded {
            self.request(Request::Page {
                session,
                delta: step,
            });
        } else {
            self.request(Request::Expand { session });
        }
        true
    }

    /// Rune 滚轮事件:dy 正是向下滚,翻到下一页。
    fn on_wheel_points(&mut self, dy: f32) -> bool {
        if dy == 0.0 {
            return false;
        }
        self.wheel_step(if dy > 0.0 { 1 } else { -1 })
    }

    /// 右键:固定条塞不下菜单,先切展开态,菜单一帧后锚在整词行上;
    /// 竖排与已展开的棋盘直接锚在命中行/格。
    fn on_secondary(&mut self, x: f32, y: f32) {
        if !self.interactive() {
            return;
        }
        let Some(geom) = &self.geom else {
            return;
        };
        let meas: &dyn Measure = self;
        let hit = panel::hit_test(&geom.spec, &geom.metrics, meas, x, y);
        let strip = matches!(geom.spec.layout, panel::Layout::Wrap) && !geom.spec.expand;
        // 全局候选序号:固定条/竖排按页内槽位加页起点,棋盘命中即全局序号。
        let gidx = match hit {
            panel::Hit::Item(slot) => Some(self.page_start() + slot),
            panel::Hit::GridItem(index) => Some(index),
            _ => None,
        };
        let Some(gidx) = gidx else {
            return;
        };
        let Some(text) = self
            .fx
            .last
            .as_ref()
            .and_then(|last| last.view.candidates.get(gidx))
        else {
            return;
        };
        let text = text.clone();
        if strip {
            let session = self.state.active.load(Ordering::Relaxed);
            let _ = imp::handle(&self.state, Request::Expand { session });
        }
        self.fx.menu = Menu::Open {
            gidx,
            text,
            hover: None,
        };
    }

    /// 回落钩子注入的鼠标事件;屏幕物理像素转客户区后走同一路径。
    /// 返回值告诉 update_if 视图读到的状态有没有变:没变不重跑 view。
    fn on_fallback(&mut self, event: FallbackMouse, _tx: &mut Tx) -> bool {
        let (ox, oy) = (
            self.fx.placement.panel.0 as f32,
            self.fx.placement.panel.1 as f32,
        );
        match event {
            FallbackMouse::Move(x, y) => self.fallback_move(x - ox, y - oy),
            FallbackMouse::Down(x, y) => {
                self.fallback_down(x - ox, y - oy);
                true
            }
            FallbackMouse::Up => self.fx.scroll_drag.take().is_some(),
            FallbackMouse::Right(x, y) => {
                self.on_secondary(x - ox, y - oy);
                true
            }
            FallbackMouse::Wheel(delta) => {
                self.wheel_step(if delta > 0 { -1 } else { 1 });
                false
            }
        }
    }

    fn fallback_move(&mut self, x: f32, y: f32) -> bool {
        if self.fx.fading() {
            return false;
        }
        // 拖动滚动条:鼠标可能移出窗口,坐标按回落钩子继续喂。
        if self.fx.scroll_drag.is_some() {
            self.scroll_drag_to(y);
            return false;
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
            return true;
        }
        false
    }

    fn fallback_down(&mut self, x: f32, y: f32) {
        let inter = self.interactive();
        if !inter {
            return;
        }
        let raw = self.raw_hit(x, y);
        // 滚动条优先:滑块直接拖,轨道空白处跳转后继续拖;⌄ 区域除外。
        let grabbed = !matches!(raw, panel::Hit::Expand) && self.scroll_grab_at(x, y);
        if grabbed {
            return;
        }
        // 菜单项优先于候选行。
        if let Menu::Open { gidx, .. } = self.fx.menu
            && let Some(geom) = &self.geom
            && let Some(rect) = menu_rect(gidx, self, geom)
            && let Some(item) = panel::menu_hit(rect.left, rect.top, &geom.metrics, x, y)
        {
            self.menu_click(item);
            return;
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
        let menu_hover = if let Menu::Open { gidx, .. } = self.fx.menu {
            menu_rect(gidx, self, geom)
                .and_then(|r| panel::menu_hit(r.left, r.top, &geom.metrics, x, y))
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
            let hit = panel::scrollbar_hit(scroll, geom.width, geom.height, &geom.metrics, x, y);
            let hit = hit?;
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

/// 采样面板上下两条窄带的桌面像素均值,算出釉光的环境色;贴屏边
/// 采不到的带子用另一条补,两条都没有就哑光(釉光清空)。
#[cfg(windows)]
fn capture_glaze(s: &mut Candidate) -> bool {
    const BAND: i32 = 6;
    let (x, y, w, h) = s.fx.placement.panel;
    if w <= 0 || h <= 0 {
        return false;
    }
    let average = |pixels: Option<(u32, u32, Vec<u8>)>| -> Option<u32> {
        let rgba = pixels?.2;
        let count = rgba.len() / 4;
        if count == 0 {
            return None;
        }
        let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
        for pixel in rgba.as_chunks::<4>().0 {
            r += u64::from(pixel[0]);
            g += u64::from(pixel[1]);
            b += u64::from(pixel[2]);
        }
        let (r, g, b) = (
            (r / count as u64) as u32,
            (g / count as u64) as u32,
            (b / count as u64) as u32,
        );
        Some((r << 16) | (g << 8) | b)
    };
    let work = place::work_area_at(x, y + h);
    let above = (y - BAND >= work.top)
        .then(|| average(capture_rect(x, y - BAND, w as u32, BAND as u32)))
        .flatten();
    let below = (y + h + BAND <= work.bottom)
        .then(|| average(capture_rect(x, y + h, w as u32, BAND as u32)))
        .flatten();
    let (above, below) = match (above, below) {
        (Some(top), Some(bottom)) => (top, bottom),
        (Some(one), None) | (None, Some(one)) => (one, one),
        (None, None) => return s.glaze.take().is_some(),
    };
    let glaze = lueying_server::theme::glaze_from_ambient(
        lueying_server::theme::srgb(above),
        lueying_server::theme::srgb(below),
    );
    let changed = s.glaze != Some(glaze);
    s.glaze = Some(glaze);
    changed
}

#[cfg(not(windows))]
fn capture_glaze(_: &mut Candidate) -> bool {
    false
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
        .role("button")
        .label(candidate.to_string())
        // key 持久身份:无 key 的格子 id 会被回收,撞上退出中节点的
        // id 就继承它在途的动画位置(日志实测 486)——整个格子带字从
        // 右边滑回来。同 key 让条↔棋盘是同一节点。不配 motion:格
        // 子矩形瞬间到位,字的飞行落点不中途挪动,路径保持直线。
        .key(format!("cell-{}-{index}-{candidate}", s.comp))
        .press_on_down()
        .on_press(move |s: &mut Candidate| s.pick_index(index));
    // 格子本身透明不可见,不需要飞;真正"哪来回哪去"的是字。
    // 与条侧同一结构(cell > 字):文字 key 两侧一致,节点跨翻转
    // 持久,布局弹簧把它从当前位置滑到棋盘位。居中用实测宽度,
    // 不走 row 的 grow——行内文字的纵向会瞬时到位,飞不全。
    let shown_w = s.text_width(&shown, geom.metrics.text);
    let text_x = (((rect.width() - shown_w) / 2.0).max(8.0 * scale)).floor();
    cell.child(
        text(shown)
            .absolute(text_x, ink_y(rect.height(), geom.metrics.text))
            .key(format!("word-{}-{index}-{candidate}", s.comp))
            .font(TextStyle::new(geom.metrics.text, 400.0))
            .color(if selected {
                look.selected_ink
            } else {
                rgb(look.palette.text)
            })
            .a11y_hidden(),
    )
}
