//! 候选窗:Rune overlay 承载窗口、玻璃与输入;布局、命中与分页语义
//! 仍来自 `lueying_render::panel`,摆位共用 `ui::place`。
//!
//! 玻璃走自捕获背板:逐帧 BitBlt 窗口区的桌面像素作为背景图,面板
//! `backdrop_blur` 出磨砂玻璃;瓷面不透明、不抓背板,釉光来自面板
//! 上下窄带的环境采样。「动态效果」关掉时回退实底主题。窗口
//! 移动、隐藏与淡出全走 Rune 的 OverlayHandle;管道线程经版本号 +
//! `kick()` 唤醒本线程,鼠标回落事件经 `OverlayHandle::send_pointer` 注入
//! Rune 自己的命中/悬停/按下/拖动管线。
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
    Align, App, AppSpec, Color, Cursor, Edge, El, LineCap, OverlayHandle, OverlayInput,
    OverlayOptions, OverlayPlacement, OverlaySpec, OverlaySupport, Pixels, Shape, Side, Spring,
    SurfaceFit, TextStyle, Transition, Tx, anchor_rect, canvas, col, el, row, text,
};
use rune_text::TextSystem;

use lueying_server::config::{UiLayout, UiMaterial};

use crate::imp;
use crate::ui::place;

/// 活背板的重捕节奏(~30fps):背景变动跟随,再高肉眼无感只费电。
const LIVE_TICK: Duration = Duration::from_millis(33);
/// 外壳 SETTLE 弹簧收尾前,收起时先沿用上一张更大的背板光学。
const GLASS_COLLAPSE_HOLD: f64 = 0.45;
/// 瓷釉环境采样的重采节奏:周边变了釉光跟着变,又不必像玻璃那样逐帧跟。
/// 捕获排除失败后的低频复查;没有新能力就不做 BitBlt。
const CAPTURE_RETRY: Duration = Duration::from_secs(1);
/// 候选文字最大宽度(逻辑像素):窗口宽度减去页码与翻页箭头的空间。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 组合结束后的淡出时长;期间沿用最后一份内容并让鼠标穿透。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 可疑左移光标的复核窗口:盖过两帧,期间有更新的报告就作废。
const CARET_HOLD: Duration = Duration::from_millis(40);

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
    /// (全局候选序号, 行文本)。行文本变了菜单自动失效。
    Open { gidx: usize, text: String },
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
    /// 本帧的逻辑钟(poll 的 t,秒):选中胶囊积分、淡出进度、捕获节流
    /// 同走一口钟,headless 下与虚拟时钟一致,不再各摸各的墙钟。
    now: f64,
    /// 收起保持:过期时间、该次提交的窗口矩形与收起前的大采样区;
    /// 区内背板照常直播。
    glass_collapse: Option<GlassCollapse>,
    /// 挂起中的可疑光标:锚点左移的报告挂一拍复核,到期无人覆盖才应用。
    caret_hold: Option<CaretHold>,
    /// 上一份内容的 sel 矩形(逻辑点,面板本地);比 view 数据可靠——
    /// 「我→我们」这类纯文字替换也会让胶囊变宽,view.highlight 不变。
    sel_rect: Option<Rect>,
    /// 形态切换时内容与胶囊同帧落位,只让外壳过渡。
    form_changed: bool,
}

/// 棋盘滑块的果冻弹簧:后缘(相对行进方向,下行是上沿)走更慢的
/// 欠阻尼弹簧——行进中它滞后,滑块被拉长;前缘停稳后它才过冲,
/// 就是撞击那瞬的单边压缩,随后回弹复圆。后缘若比前缘快,行进中
/// 就会提前过冲,变成「两边往中间挤」。交给元素布局动画
/// (rt.animating 车道)跑,帧必到,不押在会被 overlay 宿主滤掉
/// 的定时器唤醒上。
const THUMB_LEAD: rune::Spring = rune::Spring::new(0.30, 0.0);
const THUMB_TRAIL: rune::Spring = rune::Spring::new(0.50, 0.5);

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

/// 一帧的视觉参数:调色板与选中胶囊的实色;控件树的各构件共用。
struct Look<'a> {
    palette: &'a panel::Palette,
    selected_fill: Color,
    selected_ink: Color,
    high_contrast: bool,
}

impl Look<'_> {
    /// 悬停/滚动条这类覆盖层在普通材质上半透明;高对比一律走实色,不进 tint。
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

/// 候选窗 overlay 的状态;Rune 每帧从它重建控件树。
struct Candidate {
    state: Arc<imp::State>,
    fx: Fx,
    /// 文本测量;候选条的自然宽度在显示前就要量好。
    ink: RefCell<TextSystem>,
    /// 玻璃背板:流式像素通道,用 `Pixels::replace` 原地换桌面像素。
    backdrop: Option<Pixels>,
    backdrop_read: Option<(f64, (i32, i32, i32, i32))>,
    /// 上一帧 `Pixels::replace` 返回的旧 buffer,交给下一次抓屏复用。
    backdrop_spare: Vec<u8>,
    /// 釉光的环境光:面板上下窄带的桌面实采均值,瓷釉光泽与玻璃棱缘
    /// rim 共用,没有来源就不上光。
    /// 外观扩展的主题色覆盖;None 用系统主题。
    script_accent: Option<u32>,
    /// 上一次见到的视图版本号。
    version: u64,
    /// 候选窗材质;配置重载后随版本号生效。
    material: UiMaterial,
    /// 玻璃磨砂程度 0..=1;高透到磨砂连续可调。
    frost: f32,
    /// 生效的折射带宽:变体默认或 `ui_glass_band` 覆盖值(临时调参)。
    /// 配置里的主题色下标;变化时让调色板用弹簧渐变过去。
    accent: usize,
    /// 当前系统外观;约 1 秒复查,变化即让 view 重跑(即使没有背板要清)。
    appearance: lueying_server::theme::Appearance,
    /// 系统透明效果开关;关闭时玻璃退实底,瓷釉与显式光源照常。
    transparency: bool,
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
            backdrop_read: None,
            backdrop_spare: Vec::new(),
            script_accent: None,
            version: 0,
            material: UiMaterial::Glass,
            frost: 0.0,
            accent: 0,
            appearance: lueying_server::theme::Appearance::of_system(),
            transparency: lueying_server::theme::transparency_enabled(),
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

    /// 玻璃背板是否启用:动态效果开着、系统允许透明且系统没接管外观。
    fn glass_wanted(&self, high_contrast: bool) -> bool {
        !high_contrast && self.effects() && self.transparency
    }

    fn live_glass(&self, high_contrast: bool) -> bool {
        !self.private
            && self.material.is_glass()
            && self.visible
            && !self.fx.fading()
            && self.glass_wanted(high_contrast)
    }

    /// 丢弃玻璃画面与原始背板;采样失败时保留读时刻,下一拍继续重试。
    fn drop_backdrop(&mut self) -> bool {
        self.fx.glass_collapse = None;
        let changed = self.backdrop.take().is_some();
        self.backdrop_spare.clear();
        changed
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

/// 当前帧的调色板:玻璃复用共享的烟熏玻璃墨色,瓷换共享黑瓷胎体,
/// 实底跟系统主题。高对比度一律让位系统色,材质的墨色覆盖全部跳过。
fn palette_of(s: &Candidate, accent: usize, high_contrast: bool) -> panel::Palette {
    let look = if high_contrast {
        lueying_server::theme::Appearance::HighContrast
    } else {
        lueying_server::theme::Appearance::Dark
    };
    let colors = match s.script_accent {
        Some(v) if !high_contrast => lueying_server::theme::colors_with_accent(look, v),
        _ => lueying_server::theme::colors_with(look, accent),
    };
    let palette = lueying_server::theme::palette(colors);
    match s.material {
        UiMaterial::Ceramic if !high_contrast => {
            lueying_server::theme::ceramic_panel_palette(palette)
        }
        UiMaterial::Glass if !high_contrast => lueying_server::theme::glass_panel_palette(palette),
        _ => palette,
    }
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
    let ((ox, oy), ps) = place::primary_basis();
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
        // 隐藏即归还 GPU 表面(上游 release_gpu_when_hidden):复显时 surface
        // 与 lens 的 FxRenderer 都会重建,首帧必是新内容,不闪上一组句的候选页
        // 或展开棋盘;device 仍按上游策略保留五分钟,避免每次复显重建设备。
        // (prewarm 与它互斥,预热效果留不住,不配。)
        .release_gpu_when_hidden()
        // 液态玻璃的光学 compositor:lens 折射/色散/frost 走 rune-fx 原生着色器。
        .compositor(super::lens::SurfaceFx::new())
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

/// 构造候选窗调试快照的初始状态:真实布局与内容的一份静止场景。
#[cfg(debug_assertions)]
fn shot_state(high_contrast: bool, expanded: bool, stress: bool) -> Candidate {
    let mut s = Candidate::with_state(imp::visual_state());
    s.material = UiMaterial::Ceramic;
    s.appearance = if high_contrast {
        lueying_server::theme::Appearance::HighContrast
    } else {
        lueying_server::theme::Appearance::Dark
    };
    s.transparency = false;
    s.fx.reduce = true;
    s.fx.above = Some(false);
    s.fx.since = Some(0.0);
    if stress {
        let mut cfg = s.state.config.write().unwrap();
        cfg.ui_font = lueying_server::config::UiFont::Large;
        cfg.ui_layout = UiLayout::Vertical;
    }
    let mut session = SessionView {
        raw: "lue ying".into(),
        preedit: "lueying".into(),
        page_size: 6,
        highlight: 1,
        expanded,
        candidates: [
            "掠影",
            "略影",
            "掠映",
            "月影",
            "略映",
            "掠应",
            "霞光",
            "映像",
            "光影",
            "余晖",
            "夕照",
            "琥珀",
            "行云",
            "晚风",
            "长夜",
            "流星",
            "浮光",
            "微光",
            "金秋",
            "晨光",
            "琉璃",
            "黑瓷",
            "月相",
            "星河",
            "远山",
            "归途",
            "曾在夜色中捕捉一瞬微光",
            "回声",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        ..SessionView::default()
    };
    if stress {
        session.candidates[5] =
            "曾在夜色中捕捉一瞬微光并把这段很长的候选放进列表检查截断效果".to_string();
        session.page_size = 9;
    }
    let geom = geom_of(&mut s, &session);
    // 生产里 sel_rect 由 place 在发布回合算出;冻结快照要胶囊(唯一
    // 光源)在场,按同一条函数补上,否则光照路径无灯直通。
    s.fx.sel_rect = sel_rect_of(&s, &geom);
    let rect = (0, 0, geom.width as i32, geom.height as i32);
    s.fx.placement = Placement {
        window: rect,
        panel: rect,
    };
    s.fx.last = Some(Arc::new(imp::Published {
        view: session,
        private: false,
        host_draws: false,
        composing: true,
    }));
    s.geom = Some(geom);
    s
}

/// 调试快照:独立进程渲染一帧真实候选窗并写 PNG,不读配置、不碰管道、
/// 不拉起宿主,也不接入生产 poll 或事件。只给 `--visual-candidate` 用。
#[cfg(debug_assertions)]
pub(crate) fn snap(scene: &str, output: String) -> Result<(), String> {
    let (expanded, high_contrast, stress) = match scene {
        "strip" => (false, false, false),
        "grid" => (true, false, false),
        "strip-contrast" => (false, true, false),
        "grid-contrast" => (true, true, false),
        "column-stress" => (false, false, true),
        _ => return Err(format!("未知的候选窗快照场景: {scene}")),
    };
    let s = shot_state(high_contrast, expanded, stress);
    let rect = s.fx.placement.window;
    let spec = AppSpec {
        families: ("Noto Sans SC", "Noto Sans SC"),
        ..AppSpec::new(
            "Lueying Candidate Snapshot",
            (rect.2 as f32, rect.3 as f32),
            dead_init,
            view,
        )
    };
    App::new(spec)
        .compositor(super::lens::SurfaceFx::new())
        .init(move || s)
        .args([
            "--snap".into(),
            output,
            "--scale".into(),
            "2".into(),
            "--at".into(),
            "0.35".into(),
        ])
        .try_run()
}

/// 调试动效轮询:虚拟钟跨过 0.25/1.0 时按生产 toggle 提交展开/收起。
#[cfg(debug_assertions)]
fn shot_motion_step(s: &mut Candidate, now: f64, tx: &mut Tx, moving: bool) -> bool {
    s.fx.now = now;
    let expanded = (0.25..1.0).contains(&now);
    let phase = if moving {
        (now * 95.0).round() as i32
    } else {
        0
    };
    let Some(published) = &s.fx.last else {
        return false;
    };
    if published.view.expanded == expanded {
        let expired =
            s.fx.glass_collapse
                .as_ref()
                .is_some_and(|hold| now >= hold.until);
        if expired
            || (moving
                && s.backdrop_read
                    .is_none_or(|(last, _)| now - last >= LIVE_TICK.as_secs_f64()))
        {
            return shot_motion_surface(s, phase);
        }
        return false;
    }
    let mut published = (**published).clone();
    published.view.expanded = expanded;
    let geom = geom_of(s, &published.view);
    let (x, wy, ww, wh) = s.fx.placement.window;
    let h = geom.height as i32;
    let next = Placement {
        window: (x, wy, ww, wh),
        panel: (
            x,
            if s.fx.above == Some(true) {
                wy + wh - h
            } else {
                wy
            },
            geom.width as i32,
            h,
        ),
    };
    let instant = s.fx.reduce;
    begin_glass_collapse(s, next, expanded, instant);
    s.fx.placement = next;
    s.geom = Some(geom);
    s.fx.last = Some(Arc::new(published));
    s.fx.form_changed = true;
    shot_motion_surface(s, phase);
    tx.animated();
    true
}

#[cfg(debug_assertions)]
fn shot_motion_poll(s: &mut Candidate, now: f64, tx: &mut Tx) -> bool {
    shot_motion_step(s, now, tx, false)
}

/// 飞行场景:条态不换形态,0.3/0.9/1.5 秒换高亮(1→4→0→2),
/// 胶囊弹簧飞行是黑瓷光照「光跟灯走」的终极目验——灯位每帧
/// 取自元件矩形,飞行途中光池必须贴着胶囊走。
#[cfg(debug_assertions)]
fn shot_flight_poll(s: &mut Candidate, now: f64, tx: &mut Tx) -> bool {
    s.fx.now = now;
    let highlight = if now < 0.3 {
        1
    } else if now < 0.9 {
        4
    } else if now < 1.5 {
        0
    } else {
        2
    };
    let Some(published) = &s.fx.last else {
        return false;
    };
    if published.view.highlight == highlight {
        return false;
    }
    let mut published = (**published).clone();
    published.view.highlight = highlight;
    s.fx.last = Some(Arc::new(published.clone()));
    // 胶囊矩形来自 fx.sel_rect(发布路径算出),harness 改高亮
    // 必须按同一条函数补算,否则胶囊永远停在初始槽。
    let geom = geom_of(s, &published.view);
    s.fx.sel_rect = sel_rect_of(s, &geom);
    s.geom = Some(geom);
    tx.animated();
    true
}

#[cfg(debug_assertions)]
fn shot_live_motion_poll(s: &mut Candidate, now: f64, tx: &mut Tx) -> bool {
    shot_motion_step(s, now, tx, true)
}

/// 合成条纹背板:图案按桌面坐标锚定,换表面不重置;面板外不铺像素,
/// 仍由真实背景透出(不造假桌面)。
#[cfg(debug_assertions)]
fn shot_motion_surface(s: &mut Candidate, phase: i32) -> bool {
    let (x, y, w, h) = s.fx.placement.panel;
    let (_, wy, _, wh) = s.fx.placement.window;
    let y = if s.fx.above == Some(true) {
        wy + wh - h
    } else {
        y
    };
    s.fx.placement.panel = (x, y, w, h);
    if !s.material.is_glass() {
        return false;
    }
    let Some(area) = backdrop_area(s) else {
        return false;
    };
    let (x, y, w, h) = area.rect;
    s.backdrop_read = Some((s.fx.now, area.rect));
    let pad = capture_pad(s.scale);
    let rw = (w + 2 * pad) as u32;
    let rh = (h + 2 * pad) as u32;
    let mut raw = Vec::with_capacity((rw * rh * 4) as usize);
    for row in 0..rh as i32 {
        for col in 0..rw as i32 {
            let sx = x - pad + col;
            let sy = y - pad + row + phase;
            let horizontal = sy.div_euclid(18).rem_euclid(2) == 0;
            let v = if horizontal { 225 } else { 65 };
            let rgb = if sx < 160 {
                [v, v, v]
            } else if sx < 320 {
                [v, v / 2, v / 3]
            } else {
                [v / 3, v / 2, v]
            };
            raw.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    apply_backdrop(s, area, pad, rw, rh, raw)
}

/// 调试动效帧带:窗口固定为横排棋盘最大高度,只改变窗口内的面板矩形。
/// 时序来自 headless 的合成虚拟钟,证明动效路径,不代表真实输入。
/// `glass_up` 变体用合成条纹桌面作背板,面板钉在窗口底边上扩;
/// `moving` 让合成桌面逐帧滚动,验证收起保持期内背板仍直播。
#[cfg(debug_assertions)]
pub(crate) fn snap_motion(
    reduce: bool,
    glass_up: bool,
    moving: bool,
    flight: bool,
    output: String,
    frames: String,
) -> Result<(), String> {
    let mut s = shot_state(false, false, false);
    s.fx.reduce = reduce;
    let (x, y, w, _) = s.fx.placement.window;
    s.fx.placement.window = (x, y, w, WRAP_WINDOW_H as i32);
    if glass_up {
        s.material = UiMaterial::Glass;
        s.fx.above = Some(true);
        s.transparency = true;
        shot_motion_surface(&mut s, 0);
    }
    // 时序来自 headless 虚拟钟;这是动效路径的证据,不是真实输入。
    let spec = AppSpec {
        families: ("Noto Sans SC", "Noto Sans SC"),
        poll: Some(if moving {
            shot_live_motion_poll
        } else if flight {
            shot_flight_poll
        } else {
            shot_motion_poll
        }),
        ..AppSpec::new(
            "Lueying Candidate Motion",
            (w as f32, WRAP_WINDOW_H),
            dead_init,
            view,
        )
    };
    App::new(spec)
        .compositor(super::lens::SurfaceFx::new())
        .init(move || s)
        .args([
            "--strip".into(),
            output,
            "--out".into(),
            frames,
            "--scale".into(),
            std::env::var("LYSCALE").unwrap_or_else(|_| "1".into()),
            "--frames".into(),
            std::env::var("LYFRAMES").unwrap_or_else(|_| "102".into()),
            "--fps".into(),
            "60".into(),
            "--every".into(),
            "3".into(),
            "--cols".into(),
            "4".into(),
        ])
        .try_run()
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

/// 屏幕物理像素 → overlay 本地逻辑点;overlay 隐藏/未建好时为 None。
pub(crate) fn pointer_local(screen: (i32, i32)) -> Option<(f32, f32)> {
    HANDLE.get()?.screen_to_local(screen)
}

/// 把外部指针事件送进 Rune 原生命中管线;overlay 起来之前丢弃。
pub(crate) fn send_pointer(event: rune::PointerInput) {
    if let Some(handle) = HANDLE.get() {
        handle.send_pointer(event);
    }
}

/// 每帧开头跑一次:补丁、版本、摆位、活背板,并决定下一次唤醒。
fn poll(s: &mut Candidate, now: f64, tx: &mut Tx) -> bool {
    // 全窗一口钟:poll 的 t 是 Rune 帧钟,view 里所有采样都读它。
    s.fx.now = now;
    let mut changed = false;
    // client_area_animation 约 1 秒缓存;可见窗口每秒复查一次,运行中
    // 就能响应系统偏好。减动效与配置里的 effects 一起喂给 Rune。
    let reduce = !lueying_server::theme::client_area_animation();
    if s.fx.reduce != reduce {
        s.fx.reduce = reduce;
        changed = true;
    }
    let appearance = lueying_server::theme::Appearance::of_system();
    // 外观变化本身就让 view 重跑:哪怕当前没有旧背板需要清,墨色与
    // 高对比度的分流也必须重新提交。
    if appearance != s.appearance {
        s.appearance = appearance;
        changed = true;
    }
    let transparency = lueying_server::theme::transparency_enabled();
    if transparency != s.transparency {
        s.transparency = transparency;
        changed = true;
    }
    let high_contrast = matches!(
        s.appearance,
        lueying_server::theme::Appearance::HighContrast
    );
    // 私密判定在任何屏幕采样之前:活动会话一旦转私密,已经抓到的
    // 背板立刻作废,不能等到下一次 place 才停采。
    let private = s
        .state
        .active_published()
        .is_some_and(|published| published.private);
    if private != s.private {
        s.private = private;
        changed = true;
    }
    // 即时档由同一份判定驱动 place 与选中物理,避免两处口径漂移。
    let instant_motion = high_contrast || s.fx.reduce || !s.effects();
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
    let version = s.state.version.load(Ordering::Acquire);
    if version != s.version {
        s.version = version;
        let cfg = s.state.config.read().unwrap_or_else(|p| p.into_inner());
        let material = cfg.ui_material;
        let frost = cfg.ui_glass_frost.clamp(0.0, 1.0);
        let accent = cfg.ui_accent;
        drop(cfg);
        // 配置换了外观:调色板与材质参数走官方的主题变化弹簧渐变过去。
        if s.material != material || s.frost != frost || s.accent != accent {
            s.material = material;
            s.frost = frost;
            s.accent = accent;
            tx.color_motion(motion::CALM);
        }
        place(s, tx, instant_motion);
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
            // 捕获排除未明确可用就不 BitBlt;没有新能力时只按窗口节流
            // 复查,旧截图立即作废,绝不把候选窗自己录进背板。
            if super::material::desktop_capture_ready(HANDLE.get()) {
                changed |= capture_backdrop(s);
            } else {
                changed |= s.drop_backdrop();
            }
        }
        if let Some((last, _)) = s.backdrop_read {
            let failed = matches!(
                HANDLE.get().map(OverlayHandle::capture_exclusion),
                Some(OverlaySupport::Failed | OverlaySupport::Unsupported)
            );
            tx.wake_at(
                last + if failed {
                    CAPTURE_RETRY.as_secs_f64()
                } else {
                    LIVE_TICK.as_secs_f64()
                },
            );
        }
    } else if s.private || !s.glass_wanted(high_contrast) || !s.material.is_glass() {
        s.backdrop_read = None;
        changed |= s.drop_backdrop();
    }
    // 可见窗口每秒复查外观/透明/减动效;隐藏后不常驻轮询。
    if s.visible {
        tx.wake_after(Duration::from_secs(1));
    }
    changed
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
fn place(s: &mut Candidate, tx: &mut Tx, instant_motion: bool) {
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
    // 新组句还没收到光标时,不复显上一轮窗口,也不提前启动入场。
    if view.caret.is_none() {
        s.fx.clear();
        park(s);
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
        s.fx.glass_collapse = None;
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
    // 选中矩形变化(换高亮、文字变宽、条↔棋盘同位重排):同页同形态
    // 内让胶囊带着当前速度 retarget;页变/形态变直接复位,不跨旧形态
    // 拉桥。即时档一律落位,不把弹簧尾巴带进画面。
    // 选中矩形只是记录:胶囊元素 keyed 持久,矩形渐变由 Rune 原生
    // motion 推进,不再需要手写弹簧的 retarget/reset 管理。
    let cur_sel = sel_rect_of(s, &geom);
    if cur_sel != s.fx.sel_rect {
        s.fx.sel_rect = cur_sel;
    }
    let placement = if s.fx.caret_hold.is_some() {
        // 挂起回合:沿用上一回合摆位,内容照常更新,到期复核再挪。
        s.fx.placement
    } else {
        rect_for_caret(s, view, &geom)
    };
    if placement.panel.2 == 0 {
        // 独占全屏:停靠窗口,并清掉冻结内容,
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
    s.fx.form_changed = toggled;
    if toggled {
        begin_glass_collapse(s, placement, view.expanded, instant_motion);
        tx.animated();
        // 槽位重排后旧的悬停坐标已失效;箭头在棋盘里换了格子。
        s.fx.hover = None;
        s.fx.chev = false;
    }
    // 只有固定条翻页才滑动;展开棋盘里挪高亮不横向跳。上一帧也得是
    // 固定条:展开→收起那拍页码常变,武装了只会在收起后的重挂载里
    // 回放。武装单拍有效:下一提交页码没再变就清掉,不然残留期间
    // 任何视口重挂载(选中层来去都会顶动它)都把推入再播一遍,
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
    } else {
        s.fx.slide_edge = None;
    }
    // 同形态、同页内选中迁移由胶囊弹簧自己流;页变走推入、形态变走
    // 形变,各自已 animated。
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

/// 背板覆盖区:一次采样覆盖的屏幕物理矩形与当时圆角;收起保持期沿用旧的大区。
#[derive(Clone, Copy)]
struct BackdropArea {
    rect: (i32, i32, i32, i32),
}

/// 收起保持:过期时间、该次提交的窗口矩形与保持的大采样区;区内背板照常直播。
struct GlassCollapse {
    until: f64,
    window: (i32, i32, i32, i32),
    area: BackdropArea,
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

/// 候选面板圆角:固定条用胶囊,展开与竖排用独立面板圆角。
fn candidate_radius(rect: (i32, i32, i32, i32), geom: &Geom) -> f32 {
    panel::surface_radius(
        geom.spec.is_strip(),
        rect.2 as f32,
        rect.3 as f32,
        geom.metrics.scale(),
    )
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
    // 竖排宽度按全量候选估,翻页时保持同一宽;横排/棋盘宽度固定,
    // 交给 panel_width。终值回写 spec,后续行矩形与命中共用。
    if matches!(spec.layout, panel::Layout::Column) {
        spec.width = panel::measure_items_width(
            view.candidates.iter().enumerate().map(|(index, text)| {
                (
                    text.as_str(),
                    view.hints.get(index).map_or("", String::as_str),
                )
            }),
            &metrics,
            s,
        );
    }
    let width = panel::panel_width(&spec, &metrics, s);
    spec.width = width;
    Geom {
        width,
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
    let ((ox, oy), ps) = place::primary_basis();
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
    s.backdrop_spare.clear();
    s.fx.sel_rect = None;
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

/// 同窗口收起且已有更高的背板:在弹簧收尾期内保持大采样区,到期重采。
fn begin_glass_collapse(s: &mut Candidate, next: Placement, expanded: bool, instant_motion: bool) {
    let retain = !expanded
        && !instant_motion
        && s.material.is_glass()
        && !s.private
        && s.transparency
        && s.fx.placement.window == next.window
        && s.backdrop
            .as_ref()
            .is_some_and(|p| p.size().1 > next.panel.3 as u32);
    s.fx.glass_collapse = if retain {
        s.geom.as_ref().map(|_geom| GlassCollapse {
            until: s.fx.now + GLASS_COLLAPSE_HOLD,
            window: next.window,
            area: BackdropArea {
                rect: s.fx.placement.panel,
            },
        })
    } else {
        None
    };
}

/// 当前背板覆盖区:收起保持期内沿用收起前的大区,其余按当前面板矩形。
fn backdrop_area(s: &mut Candidate) -> Option<BackdropArea> {
    if let Some(hold) = &s.fx.glass_collapse
        && s.fx.now < hold.until
        && !s.fx.reduce
        && s.fx.placement.window == hold.window
        && s.backdrop.is_some()
    {
        return Some(hold.area);
    }
    s.fx.glass_collapse = None;
    let rect = s.fx.placement.panel;
    Some(BackdropArea { rect })
}

/// 抓取面板矩形外扩折射带宽的桌面像素,过液体玻璃光学后作为玻璃背板;
/// 每帧重捕,背景变动跟随。面板外的空带不铺像素,直接透出真桌面。
fn capture_backdrop(s: &mut Candidate) -> bool {
    let Some(area) = backdrop_area(s) else {
        return s.drop_backdrop();
    };
    let (x, y, w, h) = area.rect;
    if w <= 0 || h <= 0 {
        return s.drop_backdrop();
    }
    let pad = capture_pad(s.scale);
    let reuse = std::mem::take(&mut s.backdrop_spare);
    let Some((raw_w, raw_h, raw)) = super::material::capture_rect(
        x - pad,
        y - pad,
        (w + 2 * pad) as u32,
        (h + 2 * pad) as u32,
        reuse,
    ) else {
        // 采样失败:旧桌面截图不能继续冒充当前玻璃。
        return s.drop_backdrop();
    };
    apply_backdrop(s, area, pad, raw_w, raw_h, raw)
}

/// 背板捕获的外扩量(物理像素):盖住 lens 棱缘折射的采样半径,棱缘
/// 才有真实桌面可采,不落到透明外边。bevel 14pt 的位移在 2x 下约
/// 28px,留到 22pt 等量;捕获成本随面积线性,这点外扩可忽略。
pub(crate) fn capture_pad(scale: f32) -> i32 {
    (22.0 * scale).ceil() as i32 + 2
}

/// 把已采样的原始像素原样写入背板流:光学全部在 GPU(lens)侧,
/// CPU 不做任何逐像素处理。桌面抓取与调试夹具共用。去重只在源
/// 像素级别:没变就不惊动 Pixels,空闲零重绘(上游没有
/// set_if_changed,去掉会退化为背板 tick 空唤醒重绘)。
fn apply_backdrop(
    s: &mut Candidate,
    area: BackdropArea,
    pad: i32,
    raw_w: u32,
    raw_h: u32,
    raw: Vec<u8>,
) -> bool {
    let _ = (area, pad);
    if s.backdrop
        .as_ref()
        .and_then(|pixels| pixels.with_rgba(|w, h, old| (w, h) == (raw_w, raw_h) && old == raw))
        .unwrap_or(false)
    {
        s.backdrop_spare = raw;
        return false;
    }
    let (changed, old) = super::material::set_stream(&mut s.backdrop, (raw_w, raw_h), raw);
    s.backdrop_spare = old;
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
    let appearance = s.appearance;
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
    // 减少动态效果用 poll 的运行时读取(s.fx.reduce);host 的建窗
    // 快照可能落后于刚改的系统设置。用户关闭特效同样走即时档,
    // 不把空间移动强加给关动效的人。
    cx.reduced_motion(s.fx.reduce || !s.effects());
    if s.fx.last.is_none() {
        return el();
    }
    let Some(geom) = &s.geom else {
        return el();
    };
    let appearance = s.appearance;
    let high_contrast = matches!(appearance, lueying_server::theme::Appearance::HighContrast);
    // 选中胶囊的实色与字色不依赖捕获,首帧就固定;真实玻璃面依赖当前这帧
    // 已采到的背板,采不到时走实底,不在空底上画半透明假玻璃。
    let glass = !s.private
        && s.material.is_glass()
        && s.glass_wanted(high_contrast)
        && s.backdrop.is_some();
    let (_, _, accent) = place::ui_options(&s.state);
    let palette = palette_of(s, accent, high_contrast);
    let scale = geom.metrics.scale();
    let dpi = s.scale;
    let radius = candidate_radius(s.fx.placement.panel, geom) / dpi;
    let mut root = el().w_full().h_full();
    // 面板矩形是窗口内的一块:条贴光标侧,展开时铺满。矩形变化交给
    // 布局弹簧,展开/收起的形变由 Rune 原生推进。
    let bx = (s.fx.placement.panel.0 - s.fx.placement.window.0) as f32 / dpi;
    let by = (s.fx.placement.panel.1 - s.fx.placement.window.1) as f32 / dpi;
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
        // 元素外扩捕获 margin:棱缘折射要在形状外沿几像素内采到真桌面,
        // 铺到棱缘采样半径为止;裁圆半径同步外扩。像素 1:1 锚贴光标侧,
        // 展开/收起与活桌面重捕都保持屏幕对齐。
        let pl = capture_pad(dpi) as f32 / dpi;
        root = root.child(super::material::rounded_surface(
            el().absolute(bx - pl, by - pl)
                .w(s.fx.placement.panel.2 as f32 / dpi + 2.0 * pl)
                .h(s.fx.placement.panel.3 as f32 / dpi + 2.0 * pl)
                .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
                .motion(motion::SETTLE)
                // 方形图像补丁垫在圆角面板下,角上会戳出来——同半径裁圆。
                .pixels_fit(backdrop, SurfaceFit::Fill)
                .surface_unscaled(true)
                .surface_anchor(
                    Align::Start,
                    if s.fx.above == Some(true) {
                        Align::End
                    } else {
                        Align::Start
                    },
                ),
            radius + pl,
        ));
    }
    let mut body = super::material::rounded_surface(
        col()
            .absolute(bx, by)
            .w(s.fx.placement.panel.2 as f32 / dpi)
            .h(s.fx.placement.panel.3 as f32 / dpi)
            // key 锚定 key 路径:sel/格子的 keyed 祖先两侧一致,条↔棋盘
            // 是同一节点——sel 才能持久化做矩形渐变拉伸。
            .key("body")
            .hit()
            // 入场:从光标侧滑入并淡入;挂载过渡只在 animated 提交里播,
            // fresh 路径显式打 tx.animated()。顺带盖住复显首帧的透明空窗。
            .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
            .motion(motion::SETTLE)
            .on_wheel(|s: &mut Candidate, e, _| s.on_wheel_points(e.dy))
            .on_secondary_press_local(|s: &mut Candidate, p, _| s.on_secondary(p.x, p.y)),
        radius,
    );
    // 玻璃的光学面:独立 effect slot 兄弟元素,盖在捕获层之上、内容
    // 之下。折射、色散、frost 与 rim 全在 rune-fx lens 的 GPU 着色器;
    // 滑杆 frost 经 effect_data 连续进 shader;slot 矩形与圆角跟随
    // 面板弹簧(同款 transition/motion),形状同步零手写代码。
    if glass && matches!(s.material, UiMaterial::Glass) {
        root = root.child(
            rune::effect(super::lens::LENS)
                .absolute(bx, by)
                .w(s.fx.placement.panel.2 as f32 / dpi)
                .h(s.fx.placement.panel.3 as f32 / dpi)
                .radius(radius)
                .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
                .motion(motion::SETTLE)
                .effect_backdrop(true)
                .effect_bleed(28.0)
                .effect_data([s.frost, 0.0, 0.0, 0.0]),
        );
    }
    match (s.material, glass) {
        // 玻璃激活:面板表面就是 lens 的输出,body 本身不画任何底——
        // 画实底会把 lens 盖住。内容(文字/选中胶囊)照常画在最上层。
        (UiMaterial::Glass, true) => {}
        // 黑瓷:不透的共享石墨胎体,同构釉面。釉面的光全部来自环境
        // 采样:面板上方亮则光泽与上缘亮,环境暗则哑光,没有凭空的光。
        (UiMaterial::Ceramic, _) if !high_contrast => {
            body = super::material::ceramic_face(body, &palette, scale)
                // 瓷面受光 primitive:rune-light 的 bevel/gloss 参数在
                // compositor 侧,这里只声明身份。
                .mark(lueying_server::lighting::MARK_SHELL);
        }
        // 背板/动效不可用:实底主题兜底,绝不露原始桌面。
        _ => {
            body = body
                .fill(rgb(palette.bg))
                .border(1.0 * scale, rgb(palette.border));
        }
    }
    let (selected_fill, selected_ink) = if high_contrast {
        (rgb(palette.selected_bg), rgb(palette.selected_text))
    } else {
        lueying_server::theme::capsule_colors(palette.accent)
    };
    let look = Look {
        palette: &palette,
        selected_fill,
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

/// 选中槽的目标矩形(面板本地逻辑点);胶囊弹簧的目标与命中/布局
/// 共用同一份 panel 计算。
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
    // 持久——移除/新增的子节点靠同一 key 原地落位,而不是重挂载(父路径
    // 一变就走重挂载;当年 sel 同理)。形态切换的提交里视口走
    // instant_changes,字不再跨条↔棋盘飞行。
    let expanded = geom.spec.expand && matches!(geom.spec.layout, Layout::Wrap);
    let scale = geom.metrics.scale();
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
    // 面板入场过渡叠着闪。key 钉住视口身份:选中胶囊在它前面来去,
    // 没 key 时它的父内 index 跟着挪=每帧重挂载,武装过的推入被反复
    // 回放,元素看着从右缘一次次飞入。
    viewport = viewport.key("viewport");
    viewport = viewport.transition(s.fx.slide_edge.map_or(Transition::none(), |edge| {
        // 推入叠加官方 blur_replace(整块模糊替换 + 轻缩放),翻页更软。
        Transition::push(edge).combined(Transition::blur_replace())
    }));
    if s.fx.form_changed {
        viewport = viewport.instant_changes();
    }
    // 条、竖排与棋盘共用同一个原生 flex-wrap 流:格子始终是 flow 的直接
    // 子节点,父路径不随形态变化;宽度/间距仍取 panel 的同一份矩形。
    let (first, cells) = if expanded {
        let cells = panel::grid_cells(&geom.spec, &geom.metrics, meas);
        (cells.first().copied(), cells)
    } else {
        let rects = panel::item_rects(&geom.spec, &geom.metrics, meas);
        (rects.first().copied(), rects)
    };
    let mut flow = row()
        .key("flow")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .flex_wrap()
        .px(first.map_or(0.0, |r| r.left))
        .py(first.map_or(0.0, |r| r.top))
        .gap(panel::GAP * scale);
    if expanded {
        for (candidate, rect) in geom.spec.grid.iter().zip(&cells) {
            flow = flow.child(cell_el(
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
        let page_start = s.page_start();
        for (index, (item, rect)) in geom.spec.items.iter().zip(&cells).enumerate() {
            flow = flow.child(item_el(
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
    viewport = viewport.child(flow);
    // 选中胶囊垫在格子下,两侧都挂 layer(父路径一致才持久);先于视口画。
    // 中心保持实心主题色,光只沿轮廓与外溢光走;高对比直接用系统
    // 选中实底/字色,不加装饰光。
    let mut layer = el().w_full().h_full();
    if let Some(rect) = s.fx.sel_rect {
        // 目标矩形直接喂给 keyed 胶囊,矩形渐变拉伸由 Rune 原生
        // motion 推进。key 带页/形态身份:跨页、条↔棋盘、布局切换
        // 是旧节点退出 + 新节点进入(淡入淡出),只有同形态内的槽间
        // 迁移做矩形飞行——正是手写弹簧时代的 reset/retarget 语义。
        let capsule = lueying_server::capsule::selection_capsule(
            rect,
            scale,
            look.selected_fill,
            look.high_contrast,
        );
        // 黑瓷里胶囊是唯一光源:标记 emitter,灯位每帧取自它的矩形,
        // 弹簧飞行时光跟着飞。玻璃模式不标——lens 光学的边缘光自足。
        let capsule = if matches!(s.material, UiMaterial::Ceramic) && !look.high_contrast {
            capsule.mark(lueying_server::lighting::MARK_EMITTER)
        } else {
            capsule
        };
        layer = layer.child(
            capsule
                .key(format!(
                    "sel-{}-{}-{:?}",
                    s.fx.last.as_ref().map(|l| l.view.page).unwrap_or(0),
                    s.fx.last.as_ref().map(|l| l.view.expanded).unwrap_or(false),
                    geom.spec.layout,
                ))
                .motion(motion::SETTLE),
        );
    }
    layer = layer.child(viewport);
    layer = layer.child(control_tree(s, geom, look));
    layer
}

/// 一个候选格的布局事实:矩形、页内序号、全列表序号与悬停态。
#[derive(Clone, Copy)]
struct Cell {
    rect: Rect,
    index: usize,
    gidx: usize,
    hovered: bool,
}

/// 固定条的一条候选:序号、文字与注音;选中垫实心主题色胶囊。
fn item_el(s: &Candidate, geom: &Geom, look: &Look, item: &Item, at: &Cell) -> El {
    let Cell {
        rect,
        index,
        gidx,
        hovered,
    } = *at;
    let scale = geom.metrics.scale();
    let mut cell = col()
        .w(rect.width())
        .h(rect.height())
        .radius(rect.height() * 0.5)
        .cursor(Cursor::Pointer)
        .role("button")
        .label(item.text.clone())
        // key 持久身份:无 key 的格子 id 会被回收,撞上退出中节点的
        // id 就继承它在途的动画位置(日志实测 486)——整个格子带字从
        // 右边滑回来。同 key 让条↔棋盘是同一节点:形态切换提交里节点
        // 原地落位,不重挂载也不飞。不配 motion:格子矩形瞬间到位。
        .key(format!("cell-{}-{gidx}-{}", s.comp, item.text))
        .press_on_down()
        .on_press(move |s: &mut Candidate| s.pick_slot(index));
    // 格子本身透明不可见,不需要动效;身份由 key 保持,形态切换时原地落位。
    // 悬停高亮:原生路径交给 El::hover 悬停叠加(零视图提交);
    // 回落注入路径没有元素悬停,仍读 fx.hover 画 fill。
    if !item.selected {
        cell = if hovered {
            cell.fill(look.tint(look.palette.hover, 0x66))
        } else {
            cell.hover(look.tint(look.palette.hover, 0x66))
        };
    }
    // 选中语义在按键当帧就成立:索引与正文用实心胶囊上的高对比字色,
    // 不跟随胶囊位置换色。未选中序号保持原弱字样式。
    let index_ink = if item.selected {
        look.selected_ink
    } else if look.high_contrast {
        rgb(look.palette.text)
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
            // 形态切换提交里原地落位。这里仍用 key 而不是 El::matched:
            // 条↔棋盘是同一节点原地改矩形,不需要配对飞行;上游 #2314
            // 修的是 matched 自身 offset/在途重匹配,不改变这个选择。
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
                .color(if item.selected {
                    look.selected_ink
                } else if look.high_contrast {
                    rgb(look.palette.text)
                } else {
                    rgb(look.palette.text_tertiary)
                }),
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
    if s.fx.form_changed {
        layer = layer.instant_changes();
    }
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
                    .fill(if look.high_contrast {
                        rgb(look.palette.bg)
                    } else {
                        look.tint(look.palette.text_tertiary, track_a)
                    }),
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
                    .fill(if look.high_contrast {
                        rgb(look.palette.text)
                    } else {
                        look.tint(look.palette.text_tertiary, thumb_a)
                    })
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
        // 箭头和 rail 是同一元素:展开+可滚时它的矩形就是整条 rail;
        // 形态切换提交里控制层同帧落位,弹簧不参与,非切换几何变化才拉伸。
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
            // 无 transition 不淡入淡出。慢弹簧只作用于非切换的几何变化:
            // 前半段被生长的 body 裁剪边盖住,后半段在面板定型后可见地
            // 拉伸;切换提交里控制层同帧落位,按钮不会延迟长成轨道。
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
                    // drag.x/y 已相对当前绘制元素(轨道)本地,加上轨道原点
                    // 即面板本地;window_point 在向上展开时含顶部留白。
                    let (x, y) = (track.left + drag.x, track.top + drag.y);
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
                rune::Phase::Move => s.scroll_drag_to(track.top + drag.y),
                rune::Phase::Up => s.fx.scroll_drag = None,
            });
        if is_rail {
            chev = chev.radius(track.width() * 0.5);
            if look.high_contrast {
                chev = chev.border(1.0 * scale, rgb(look.palette.text));
            } else {
                chev = chev
                    .fill(tint(0x000000, 0x34))
                    .border(1.0 * scale, tint(0xffffff, 0x14));
            }
        } else {
            chev = chev.radius(track.height() * 0.5);
            let alpha = match (expanded, s.fx.chev) {
                (true, true) => 0x66,
                (true, false) => 0x38,
                (false, true) => 0x4d,
                (false, false) => 0x00,
            };
            if alpha > 0 && !look.high_contrast {
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
        let (color, width) = (
            rgb(if look.high_contrast {
                look.palette.text
            } else {
                look.palette.text_secondary
            }),
            1.6 * scale,
        );
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
    let card = col()
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
    let mut items = col()
        .w_full()
        .h_full()
        .padding(4.0 * scale)
        .gap(2.0 * scale);
    for (index, label) in panel::MENU_ITEMS.iter().enumerate() {
        let row = el()
            .w_full()
            .h(26.0 * scale)
            .radius(panel::concentric_radius(10.0 * scale, 4.0 * scale, 0.0))
            .cursor(Cursor::Pointer)
            .role("menuitem")
            .label_static(label)
            .focusable(true)
            .focus_highlight(tint)
            .hover(tint)
            .press_on_down()
            .on_press(move |s: &mut Candidate| s.menu_click(index));
        items = items.child(
            row.child(
                text(*label)
                    .absolute(0.0, ink_y(26.0 * scale, 12.0 * scale))
                    .font(TextStyle::new(12.0 * scale, 400.0))
                    .color(rgb(look.palette.text))
                    .a11y_hidden(),
            ),
        );
    }
    card.child(items)
}

/// 菜单矩形(面板本地逻辑点):与 El::anchored 同一份数学(anchor_rect),
/// 回落注入路径拿它做命中与悬停;锚格不在本页/棋盘时返回 None,
/// 视图不挂菜单(anchored 解析不到锚也会压到零矩形,双保险)。
fn menu_rect(gidx: usize, s: &Candidate, geom: &Geom) -> Option<Rect> {
    let scale = geom.metrics.scale();
    let dpi = s.scale;
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
    let bx = (s.fx.placement.panel.0 - s.fx.placement.window.0) as f32 / dpi;
    let by = (s.fx.placement.panel.1 - s.fx.placement.window.1) as f32 / dpi;
    let target = [
        cell.left + bx,
        cell.top + by,
        cell.right + bx,
        cell.bottom + by,
    ];
    let win = (
        s.fx.placement.window.2 as f32 / dpi,
        s.fx.placement.window.3 as f32 / dpi,
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
        self.fx.menu = Menu::Open { gidx, text };
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

/// 棋盘的一格:选中时垫同款实心主题色胶囊,文字在格内居中。
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
        .w(rect.width())
        .h(rect.height())
        .radius(rect.height() * 0.5)
        .px(8.0 * scale)
        .cursor(Cursor::Pointer)
        .role("button")
        .label(candidate.to_string())
        // key 持久身份:无 key 的格子 id 会被回收,撞上退出中节点的
        // id 就继承它在途的动画位置(日志实测 486)——整个格子带字从
        // 右边滑回来。同 key 让条↔棋盘是同一节点:形态切换提交里节点
        // 原地落位,不重挂载也不飞。不配 motion:格子矩形瞬间到位。
        .key(format!("cell-{}-{index}-{candidate}", s.comp))
        .press_on_down()
        .on_press(move |s: &mut Candidate| s.pick_index(index));
    // 格子本身透明不可见,不需要动效;身份由 key 保持,形态切换时原地落位。
    // 与条侧同一结构(cell > 字):文字 key 两侧一致,节点跨翻转持久;
    // 切换提交里视口同帧落位,不靠布局弹簧滑到棋盘位。居中用实测宽度,
    // 不走 row 的 grow——行内文字的纵向会瞬时到位,与横向落点不同拍。
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
