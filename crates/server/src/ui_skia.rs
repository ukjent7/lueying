//! Skia 候选窗:Win32 无焦点弹出窗 + DirectComposition 交换链 + Ganesh-D3D12。
//!
//! 候选窗:独立线程自建消息泵,15ms 轮询会话版本号;摆位、停靠、DPI 与
//! 全屏避让同 `ui_place` 助手共用。
//! 交互 parity:淡入/淡出、翻页滑动、点选/滚轮/翻页箭头、右键菜单与悬停,
//! 动效可被按键打断,「减少动态效果」时全部走终态。D3D12 只用硬件适配器,
//! 建不出设备时退到 CPU 光栅 + 分层窗口(`ui_skia_cpu`),画面一模一样,
//! 只是每帧多花一点 CPU;设备中途被系统摘掉(驱动重启、GPU 挂起)也走同一条
//! 换手路:另建一个分层窗口接着弹,候选窗不会整块消失。
//! `LUEYING_SKIA_SELFTEST` 供 CI 自检整条管线。模式徽标走原生分层窗口
//! (`ui_hud`:CPU 光栅 + UpdateLayeredWindow),不在这条管线上。

use std::sync::atomic::Ordering;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use lueying_extjs::ScenePatch;
use lueying_protocol::SessionView;
use lueying_render::effect::{Budget, BudgetGuard, Slot, Uniforms};
use lueying_render::panel::{self, Metrics, PanelSpec};
use lueying_server::theme;
use skia_safe::Font;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE, GWLP_USERDATA,
    GetWindowLongPtrW, HCURSOR, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LoadCursorW, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SPI_GETCLIENTAREAANIMATION, SWP_NOACTIVATE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowLongPtrW, SetWindowPos, SystemParametersInfoW,
    TranslateMessage, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_SETCURSOR,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};
use windows::core::{BOOL, w};

use crate::imp::State;
use crate::ui_place::{self, CARET_GAP};
use crate::ui_skia_cpu::CpuPipeline;
use crate::ui_skia_effect;
use crate::ui_skia_gpu::{Frame, FrameTarget, Pipeline};
use crate::ui_skia_mouse;
use crate::ui_skia_script;

/// 会话版本号轮询间隔;与 GPUI 路径一致,对打字反馈无感。
const POLL: Duration = Duration::from_millis(15);
/// 候选文字最大宽度(逻辑像素);与 GPUI 路径的 `MAX_WIDTH - 190` 一致。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 首次出现淡入:首帧就有 45% 不透明度,140ms 内到 100%,避免快速打字时候选短暂不可见。
const SHOW_FADE: Duration = Duration::from_millis(140);
const SHOW_START_OPACITY: f32 = 0.45;
/// 组合结束后的淡出时长;期间沿用最后一份内容并让鼠标穿透。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 翻页时候选内容横向滑入,90ms 内归位。
const PAGE_SLIDE: Duration = Duration::from_millis(90);
/// 效果目录的检查间隔;用户改完 .sksl 一秒内生效。
const EFFECT_CHECK: Duration = Duration::from_millis(1000);

/// 启动 Skia 候选窗线程;建窗或建管线失败只写日志,输入法照常工作。
pub(super) fn spawn(state: Arc<State>) {
    let spawned = std::thread::Builder::new()
        .name("skia-candidate".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| window_loop(state)))
                .is_err()
            {
                println!("Skia 候选窗线程崩溃,后续组合不再有候选窗");
            }
        });
    if let Err(e) = spawned {
        println!("Skia 候选窗线程启动失败: {e}");
    }
}

/// 窗口线程的动画与交互状态;全部归窗口线程所有。
#[derive(Default)]
pub(super) struct Fx {
    /// 淡入截止时刻;`None` 表示没有在淡入。
    show: Option<Instant>,
    /// 淡出截止时刻;期间沿用最后一份内容并让鼠标穿透。
    hide: Option<Instant>,
    /// 最后一份组合内容;淡出期间冻结,平时跟最新一帧。
    last: Option<SessionView>,
    /// 正在进行的翻页滑动:方向与截止时刻。
    slide: Option<(f32, Instant)>,
    /// 上一帧的页码,用来判断翻页方向。
    page: Option<usize>,
    /// 本次组合里候选窗在光标上方还是下方;同一次组合内保持不变。
    above: Option<bool>,
    /// 系统「减少动态效果」;开启后全部动效走终态。
    reduce: bool,
    /// 页内悬停候选;鼠标移出或换页后由重绘修正。
    pub(super) hover: Option<usize>,
    /// 悬停的翻页箭头;单页时恒为 `None`。
    pub(super) pager: Option<panel::Hit>,
    /// 打开的右键菜单:(页内槽位, 菜单顶部 y, 行文本)。行文本变了菜单自动失效。
    pub(super) menu: Option<(usize, f32, String)>,
    /// 悬停的菜单项。
    pub(super) menu_hover: Option<usize>,
    /// 鼠标最后位置(客户区);WM_SETCURSOR 用它做命中,省一次坐标换算。
    pub(super) mouse: (f32, f32),
    /// 最近一帧的绘制输入;命中测试用它,保证所指即所画。
    pub(super) geometry: Option<Geom>,
    /// 新组合的第一帧走素色;之后才上效果,候选一出现就可读。
    first_frame: bool,
    /// 本次组合的起点;效果的动画时钟从这儿算。
    since: Option<Instant>,
}

impl Fx {
    /// 回到空闲态;reduce 偏好保留。
    fn clear(&mut self) {
        *self = Fx {
            reduce: self.reduce,
            ..Fx::default()
        };
    }

    /// 淡出中:窗口沿用冻结内容并让鼠标穿透。
    pub(super) fn fading(&self) -> bool {
        self.hide.is_some()
    }

    /// 效果的动画时钟:本次组合开始后的秒数;「减少动态效果」时冻结为 0。
    fn clock(&self, now: Instant) -> f32 {
        if self.reduce {
            return 0.0;
        }
        self.since
            .map(|since| now.saturating_duration_since(since).as_secs_f32())
            .unwrap_or(0.0)
    }

    /// 入场/翻页进度:静止为 1,淡出时退回 0。
    fn anim(&self, now: Instant) -> f32 {
        if let Some(end) = self.show {
            return ease_out_quint(progress(SHOW_FADE, end, now));
        }
        if let Some(end) = self.hide {
            return 1.0 - ease_out_quint(progress(HIDE_FADE, end, now));
        }
        1.0
    }
}

/// 最近一帧的绘制输入快照;鼠标命中据此换算,不重算布局。
pub(super) struct Geom {
    pub(super) spec: PanelSpec,
    pub(super) metrics: Metrics,
    pub(super) menu_xy: Option<(f32, f32)>,
}

/// 窗口过程持有的上下文;窗口与线程同生命周期,进程退出一起回收。
pub(super) struct WinCtx {
    /// 候选窗窗口;换出帧路径时会换成新窗口。
    pub(super) hwnd: HWND,
    pub(super) state: Arc<State>,
    pub(super) fx: Fx,
    pipe: Presenter,
    pub(super) font: Option<Font>,
    pub(super) cursor_arrow: Option<HCURSOR>,
    pub(super) cursor_hand: Option<HCURSOR>,
    /// 用户效果:加载期编译好,坏文件只废掉自己的槽位。
    effects: ui_skia_effect::Effects,
    /// 运行期脚本:后台线程跑 QuickJS,这里只收补丁。
    script: ui_skia_script::Script,
    /// 已应用的脚本补丁;空表示没有扩展生效。
    script_fx: Option<ui_skia_script::ScriptFx>,
    /// 效果帧预算:连续超预算就临时摘掉效果。
    budget: BudgetGuard,
    /// 出帧路径彻底走不通(设备被摘掉且 CPU 兜底也建不起来):候选窗停用。
    dead: bool,
}

/// 出帧路径:GPU 优先,没有硬件 D3D12 就退到 CPU 光栅。
///
/// 两条路径的画面完全一致(同一套绘制代码),只是上屏方式不同:GPU 走
/// DirectComposition 交换链,CPU 走 GDI 分层窗口。
pub(super) enum Presenter {
    Gpu(Pipeline),
    Cpu(CpuPipeline),
}

impl Presenter {
    pub(super) fn present(&mut self) -> Result<(), String> {
        match self {
            Presenter::Gpu(pipe) => pipe.present(),
            Presenter::Cpu(pipe) => pipe.present(),
        }
    }

    pub(super) fn rebuild_surfaces(&mut self, w: u32, h: u32) -> Result<(), String> {
        match self {
            Presenter::Gpu(pipe) => pipe.rebuild_surfaces(w, h),
            Presenter::Cpu(pipe) => pipe.rebuild_surfaces(w, h),
        }
    }

    fn device_removed(&self) -> bool {
        match self {
            Presenter::Gpu(pipe) => pipe.device_removed(),
            Presenter::Cpu(_) => false,
        }
    }
}

impl FrameTarget for Presenter {
    fn draw(&mut self, f: Frame) -> Result<(), String> {
        match self {
            Presenter::Gpu(pipe) => pipe.draw(f),
            Presenter::Cpu(pipe) => pipe.draw(f),
        }
    }

    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        match self {
            Presenter::Gpu(pipe) => pipe.pixel(x, y),
            Presenter::Cpu(pipe) => pipe.pixel(x, y),
        }
    }
}

/// 建候选窗与出帧路径。GPU 管线建不起来(没有硬件 D3D12、驱动异常)时,
/// 换一个分层窗口走 CPU 光栅,候选窗照常弹出来。
fn open() -> Result<(HWND, Presenter), String> {
    let hwnd = create_window()?;
    match Pipeline::new(hwnd) {
        Ok(pipe) => {
            println!("Skia 候选窗就绪:Ganesh-D3D12 + DirectComposition");
            Ok((hwnd, Presenter::Gpu(pipe)))
        }
        Err(e) => {
            println!("Skia 候选窗 D3D12 管线不可用: {e};改用 CPU 光栅");
            // 原来的窗口已经挂了 DComp 目标,换一个新的分层窗口。
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            let (hwnd, pipe) = cpu_presenter()?;
            println!("Skia 候选窗就绪:CPU 光栅 + 分层窗口");
            Ok((hwnd, Presenter::Cpu(pipe)))
        }
    }
}

/// CPU 光栅呈现器:分层窗口 + 位图。
pub(super) fn cpu_presenter() -> Result<(HWND, CpuPipeline), String> {
    let hwnd = create_layered_window()?;
    match CpuPipeline::new(hwnd) {
        Ok(pipe) => Ok((hwnd, pipe)),
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            Err(e)
        }
    }
}

/// 换一条出帧路径:另建一个分层窗口走 CPU 光栅,旧窗口销毁。
///
/// 设备被系统摘掉(驱动重启、GPU 挂起)时用它接手,候选窗不会整块消失。
pub(super) fn swap_window(old: HWND, pipe: &mut Presenter) -> Result<HWND, String> {
    let (new_hwnd, cpu) = cpu_presenter()?;
    // 旧 GPU 管线这就要丢:设备可能已经被摘掉,先让 Skia 放弃上下文,
    // 析构时就不会再碰后端 API(否则会访问冲突)。
    if let Presenter::Gpu(gpu) = pipe {
        gpu.abandon();
    }
    *pipe = Presenter::Cpu(cpu);
    // 淡出期间的点击穿透样式要跟着搬到新窗口。
    if ex_style(old) & WS_EX_TRANSPARENT.0 != 0 {
        style_click_through(new_hwnd, true);
    }
    unsafe {
        let _ = DestroyWindow(old);
    }
    Ok(new_hwnd)
}

/// 出帧路径的准备结果。
enum Ready {
    /// 路径照旧,直接画。
    Same,
    /// 换了路径(设备被摘掉):窗口要重新摆位、重建表面。
    Switched,
    /// 这条路彻底走不通:候选窗停用。
    Dead,
}

/// 出帧前的准备:设备被系统摘掉就换到 CPU 光栅路径。
///
/// 换了路径要重新摆位、重建表面,由调用方接着做。
fn ensure_presenter(ctx: &mut WinCtx) -> Ready {
    if !ctx.pipe.device_removed() {
        return Ready::Same;
    }
    match swap_window(ctx.hwnd, &mut ctx.pipe) {
        Ok(hwnd) => {
            ctx.hwnd = hwnd;
            // 窗口过程靠这个指针反查上下文;新窗口也要挂上。
            unsafe {
                let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as *mut WinCtx as isize);
            }
            println!("Skia 候选窗:D3D12 设备被摘掉,改走 CPU 光栅");
            Ready::Switched
        }
        Err(e) => {
            ctx.dead = true;
            println!("Skia 候选窗:D3D12 设备被摘掉,CPU 兜底也没建起来({e}),候选窗停用");
            Ready::Dead
        }
    }
}

fn window_loop(state: Arc<State>) {
    // DComp 与窗口消息都归这条线程;COM 已按别的 apartment 初始化过时忽略。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let (hwnd, pipe) = match open() {
        Ok(pair) => pair,
        Err(e) => {
            println!("Skia 候选窗初始化失败: {e}");
            return;
        }
    };
    let font = panel_font();
    let (cursor_arrow, cursor_hand) = unsafe {
        (
            LoadCursorW(None, IDC_ARROW).ok(),
            LoadCursorW(None, IDC_HAND).ok(),
        )
    };
    let script = ui_skia_script::Script::spawn(Arc::clone(&state));
    // 窗口过程要反查状态;循环常驻,泄漏的 Box 与进程同寿命。
    let ctx: *mut WinCtx = Box::leak(Box::new(WinCtx {
        hwnd,
        state: Arc::clone(&state),
        fx: Fx::default(),
        pipe,
        font,
        cursor_arrow,
        cursor_hand,
        effects: ui_skia_effect::Effects::load(&state),
        script,
        script_fx: None,
        budget: BudgetGuard::new(Budget::default()),
        dead: false,
    }));
    unsafe {
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);
    }
    let mut last_version = 0u64;
    let mut next_poll = Instant::now();
    let mut next_effect_check = Instant::now();
    loop {
        // 无焦点窗口的消息量很小,每轮顺手排干;真正的节奏靠 5ms 睡眠。
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let now = Instant::now();
        // 脚本补丁到了就应用;这一轮有新内容时由 place 一起重绘。
        let patch = unsafe { &(*ctx).script }.take();
        if let Some((seq, patch)) = patch {
            let ctx = unsafe { &mut *ctx };
            let redraw_now = state.version.load(Ordering::Relaxed) == last_version;
            apply_script(ctx, seq, patch, redraw_now);
        }
        if now >= next_poll {
            next_poll = now + POLL;
            let version = state.version.load(Ordering::Relaxed);
            if version != last_version {
                last_version = version;
                // 消息泵之外取可变借用;窗口过程里的借用都已结束。
                let ctx = unsafe { &mut *ctx };
                ctx.fx.reduce = reduce_motion();
                place(ctx);
            }
        }
        // 效果目录或开关变了就重新加载;编译在加载期做,帧内只打包 uniform。
        if now >= next_effect_check {
            next_effect_check = now + EFFECT_CHECK;
            let ctx = unsafe { &mut *ctx };
            ctx.effects.refresh(&ctx.state);
        }
        // 淡入/淡出/滑动进行中按循环节奏重绘;5ms 粒度足够 90–140ms 的动效。
        let animating = unsafe {
            let fx = &(*ctx).fx;
            fx.hide.is_some() || fx.show.is_some() || fx.slide.is_some()
        };
        if animating {
            let ctx = unsafe { &mut *ctx };
            animate(ctx);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 组合变化后重新摆位并出帧;无内容时淡出或停靠。
fn place(ctx: &mut WinCtx) {
    if ctx.dead {
        // 出帧路径没了,窗口一直停在屏幕外,不再参与摆位与绘制。
        park(ctx);
        return;
    }
    let Some(snap) = ui_place::snapshot(&ctx.state) else {
        // 组合结束:reduce 直接停靠;否则窗口留在原地淡出,到期由动画帧停靠。
        if ctx.fx.reduce || ctx.fx.last.is_none() {
            ctx.fx.clear();
            park(ctx);
            return;
        }
        if ctx.fx.hide.is_none() {
            ctx.fx.hide = Some(Instant::now() + HIDE_FADE);
            ctx.fx.show = None;
            ctx.fx.slide = None;
            ctx.fx.menu = None;
            ctx.fx.menu_hover = None;
            ctx.fx.hover = None;
            ctx.fx.pager = None;
            style_click_through(ctx.hwnd, true);
        }
        return;
    };
    let view = &snap.view;
    if snap.private {
        // 隐私字段整个扩展层停用:不等脚本线程回包,这一帧就回素色。
        ctx.script_fx = None;
    }
    let fresh = ctx.fx.last.is_none() || ctx.fx.hide.is_some();
    if fresh {
        // 新一次组合重新判断候选窗朝向;候选行数变化时不会上下翻跳。
        ctx.fx.above = None;
        ctx.fx.slide = None;
        ctx.fx.page = None;
        ctx.fx.menu = None;
        ctx.fx.menu_hover = None;
        ctx.fx.hover = None;
        ctx.fx.pager = None;
        // 新组合:第一帧走素色,效果时钟从现在起算。
        ctx.fx.first_frame = true;
        ctx.fx.since = Some(Instant::now());
        ctx.fx.show = if ctx.fx.reduce {
            None
        } else {
            Some(Instant::now() + SHOW_FADE)
        };
    }
    ctx.fx.hide = None;
    style_click_through(ctx.hwnd, false);
    // 只有翻页才滑动;首次展开和继续打字不横向跳。
    if !ctx.fx.reduce && ctx.fx.page.is_some_and(|page| page != view.page) {
        let previous = ctx.fx.page.unwrap_or(view.page);
        let direction = if view.page > previous { 1.0 } else { -1.0 };
        ctx.fx.slide = Some((direction, Instant::now() + PAGE_SLIDE));
        ctx.fx.hover = None;
        ctx.fx.pager = None;
    }
    ctx.fx.page = Some(view.page);
    let rect = caret_rect(ctx, view);
    if rect.2 == 0 {
        // 独占全屏或拿不到光标信息:停靠窗口,并清掉冻结内容,
        // 下一次组合按新组合淡入,不会接着上一次的状态。
        ctx.fx.show = None;
        ctx.fx.hide = None;
        ctx.fx.slide = None;
        ctx.fx.last = None;
        park(ctx);
        return;
    }
    // 设备被系统摘掉就先换到 CPU 光栅,再摆位出帧。
    match ensure_presenter(ctx) {
        Ready::Dead => return,
        Ready::Same | Ready::Switched => {}
    }
    if let Err(e) = layout_window(ctx, rect) {
        println!("Skia 候选窗调整失败: {e}");
        return;
    }
    // 先落内容再出帧;淡出期间用的就是这份冻结内容。
    ctx.fx.last = Some(view.clone());
    submit_scene(ctx, view, snap.private);
    draw_view(ctx, view);
}

/// 把当前面板状态交给脚本线程;补丁晚一帧到,不影响这一帧。
fn submit_scene(ctx: &WinCtx, view: &SessionView, private: bool) {
    let now = Instant::now();
    let scene = ui_skia_script::scene(
        &ctx.state,
        view,
        private,
        ctx.fx.reduce,
        ctx.fx.clock(now),
        ctx.fx.anim(now),
    );
    ctx.script.submit(scene);
}

/// 应用脚本补丁;内容没变就不重绘。
fn apply_script(ctx: &mut WinCtx, seq: u64, patch: Option<ScenePatch>, redraw_now: bool) {
    // 旧场景的补丁晚到:新一帧或隐私字段已经提交,直接丢弃。
    if seq < ctx.script.seq() {
        return;
    }
    let unchanged = match (&patch, &ctx.script_fx) {
        (Some(next), Some(current)) => current.same(next),
        (None, None) => true,
        _ => false,
    };
    if unchanged {
        return;
    }
    let previous = ctx.script_fx.take();
    ctx.script_fx = patch.map(|patch| ui_skia_script::ScriptFx::update(previous, patch));
    if redraw_now {
        redraw(ctx);
    }
}

/// 光标位置算出的候选窗矩形(x, y, 宽, 高);宽为 0 表示这次不弹。
fn caret_rect(ctx: &mut WinCtx, view: &SessionView) -> (i32, i32, i32, i32) {
    let (layout, ui_font, _) = ui_place::ui_options(&ctx.state);
    let host_pid = (ctx.state.active.load(Ordering::Relaxed) >> 32) as u32;
    let Some((l, t, r, b)) = view.caret else {
        ctx.fx.above = None;
        ctx.state.candidate_above.store(false, Ordering::Relaxed);
        return (0, 0, 0, 0);
    };
    let left = l.min(r);
    // 按光标所在显示器算 DPI;停靠屏的缩放与本屏无关。
    let scale = ui_place::scale_at(left, b);
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let mut spec = ui_place::spec_of(view, layout);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = ui_place::measure_width(view, layout, &metrics, ctx.font.as_ref());
    let mut w = panel::panel_width(&spec, &metrics, ctx.font.as_ref()) as i32;
    let h = panel::panel_height(&spec, &metrics, ctx.font.as_ref()) as i32;
    let work = ui_place::work_area_at(left, b);
    w = w.min((work.right - work.left).max(1));
    // 同一次组合内优先留在已选的一侧;只有这一侧放不下才翻边。
    // 副屏可以位于主屏上方,`top` 是负数,不能直接 `.max(0)`。
    let gap = (CARET_GAP * scale) as i32;
    let below = b + gap;
    let over = t - h - gap;
    let fits_below = below + h <= work.bottom;
    let fits_above = over >= work.top;
    let prefer_above = match ctx.fx.above {
        Some(previous) if previous && fits_above => true,
        Some(previous) if !previous && fits_below => false,
        _ => !fits_below && fits_above,
    };
    ctx.fx.above = Some(prefer_above);
    // 徽标据此躲到候选窗的另一侧,两者不互相遮挡。
    ctx.state
        .candidate_above
        .store(prefer_above, Ordering::Relaxed);
    let y =
        (if prefer_above { over } else { below }).clamp(work.top, (work.bottom - h).max(work.top));
    let x = left.clamp(work.left, (work.right - w).max(work.left));
    // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
    let hidden = ui_place::foreground_fullscreen_at(left, b, host_pid);
    let w = if hidden { 0 } else { w };
    (x, y, w, h)
}

/// 摆位并把表面尺寸对上;GPU 与 CPU 两条路径同一套。
fn layout_window(ctx: &mut WinCtx, rect: (i32, i32, i32, i32)) -> Result<(), String> {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return Err("候选窗尺寸为空".to_string());
    }
    unsafe {
        let _ = SetWindowPos(ctx.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    }
    ctx.pipe.rebuild_surfaces(w as u32, h as u32)
}

/// 动画帧:淡入/淡出/滑动进行中重绘;淡出到期停靠,其余到期走终态。
fn animate(ctx: &mut WinCtx) {
    if ctx.dead {
        park(ctx);
        return;
    }
    let now = Instant::now();
    if ctx.fx.hide.is_some_and(|end| now >= end) {
        ctx.fx.clear();
        style_click_through(ctx.hwnd, false);
        park(ctx);
        return;
    }
    // 到期的动画清掉;终态与上一帧在视觉上已重合,下轮不再进动画分支。
    if ctx.fx.show.is_some_and(|end| now >= end) {
        ctx.fx.show = None;
    }
    if ctx.fx.slide.is_some_and(|(_, end)| now >= end) {
        ctx.fx.slide = None;
    }
    let animating = ctx.fx.hide.is_some() || ctx.fx.show.is_some() || ctx.fx.slide.is_some();
    let last = if animating { ctx.fx.last.clone() } else { None };
    if let Some(view) = last {
        draw_view(ctx, &view);
    }
}

/// 出一帧:透明度(淡出 > 淡入)、列表动效、悬停与菜单,全部换算后交给管线。
fn draw_view(ctx: &mut WinCtx, view: &SessionView) {
    if ctx.dead {
        return;
    }
    // 设备被系统摘掉(驱动重启、GPU 挂起)后不能再进 Skia:命令表已经建不出来,
    // Skia 拿着空句柄继续走会崩。换到 CPU 光栅路径接着弹,候选窗不会消失。
    match ensure_presenter(ctx) {
        Ready::Dead => return,
        Ready::Same => {}
        Ready::Switched => {
            // 新窗口要重新摆位、重建表面,才能接着画这一帧。
            let rect = caret_rect(ctx, view);
            if let Err(e) = layout_window(ctx, rect) {
                println!("Skia 候选窗调整失败: {e}");
                return;
            }
        }
    }
    let (layout, ui_font, accent) = ui_place::ui_options(&ctx.state);
    let appearance = theme::Appearance::of_system();
    // 高对比度由系统接管外观,用户效果整个关掉,免得压过系统配色。
    let high_contrast = matches!(appearance, theme::Appearance::HighContrast);
    // 脚本主题色只换主色与选中底色;高对比度仍用系统色。
    let colors = match ctx.script_fx.as_ref().and_then(|fx| fx.accent()) {
        Some(rgb) if !high_contrast => theme::colors_with_accent(appearance, rgb),
        _ => theme::colors_with(appearance, accent),
    };
    let palette = theme::palette(colors);
    // 摆位时已把窗口放到光标屏;动效重绘沿用同一份缩放。
    let scale = match view.caret {
        Some((l, _, r, b)) => ui_place::scale_at(l.min(r), b),
        None => 1.0,
    };
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let mut spec = ui_place::spec_of(view, layout);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = ui_place::measure_width(view, layout, &metrics, ctx.font.as_ref());
    let now = Instant::now();
    let opacity = if let Some(end) = ctx.fx.hide {
        (1.0 - ease_out_quint(progress(HIDE_FADE, end, now))).max(0.0)
    } else if let Some(end) = ctx.fx.show {
        SHOW_START_OPACITY
            + (1.0 - SHOW_START_OPACITY) * ease_out_quint(progress(SHOW_FADE, end, now))
    } else {
        1.0
    };
    let list = match ctx.fx.slide {
        Some((direction, end)) if now < end => {
            let delta = ease_out_quint(progress(PAGE_SLIDE, end, now));
            panel::ListFx {
                dx: direction * 10.0 * metrics.scale() * (1.0 - delta),
                opacity: 0.55 + 0.45 * delta,
            }
        }
        _ => panel::ListFx::NEUTRAL,
    };
    // 菜单只挂在内容未变的行上;翻页或继续打字后行文本对不上,菜单自动失效。
    let menu = ctx.fx.menu.as_ref().and_then(|(slot, _, text)| {
        spec.items
            .get(*slot)
            .is_some_and(|item| &item.text == text)
            .then(|| menu_origin(*slot, &spec, &metrics, ctx.font.as_ref()))
    });
    if menu.is_none() {
        ctx.fx.menu = None;
        ctx.fx.menu_hover = None;
    }
    ctx.fx.geometry = Some(Geom {
        spec: spec.clone(),
        metrics,
        menu_xy: menu,
    });
    // 效果:按 uniform 生成三个槽位的 shader。新组合的第一帧走素色,
    // 保证候选一出现就可读;帧耗时超预算、开了高对比度时也临时摘掉效果。
    let width = panel::panel_width(&spec, &metrics, ctx.font.as_ref());
    let height = panel::panel_height(&spec, &metrics, ctx.font.as_ref());
    // 画布单位与逻辑单位(设计像素)的比例;效果坐标按它换算。
    let unit = metrics.scale();
    let accent = Uniforms::accent_from_rgb(palette.accent);
    let time = ctx.fx.clock(now);
    let anim = ctx.fx.anim(now);
    // 三个槽位各自在自己的区域里作画:坐标从区域左上角起算,单位是逻辑单位。
    let rects = panel::item_rects(&spec, &metrics, ctx.font.as_ref());
    let selected = spec
        .items
        .iter()
        .position(|item| item.selected)
        .and_then(|index| rects.get(index))
        .copied();
    let allowed = !ctx.fx.first_frame && !high_contrast && ctx.budget.allows(now);
    let (background, highlight, menu_effect) = if allowed {
        let panel_uniforms = Uniforms {
            size: [width / unit, height / unit],
            time,
            anim,
            accent,
        };
        let mut highlight_uniforms = panel_uniforms;
        if let Some(rect) = selected {
            highlight_uniforms.size = [rect.width() / unit, rect.height() / unit];
        }
        let mut menu_uniforms = panel_uniforms;
        if menu.is_some() {
            menu_uniforms.size = [116.0, panel::menu_height(&metrics) / unit];
        }
        let script = ctx.script_fx.as_ref();
        let file = ctx.effects.set();
        // 脚本补丁优先;没给或编译失败的槽位落到用户效果目录。
        let shader = |slot: Slot, uniforms: &Uniforms, origin: (f32, f32)| {
            script
                .and_then(|fx| fx.shader(slot, uniforms, origin, unit))
                .or_else(|| file.shader(slot, uniforms, origin, unit))
        };
        (
            shader(Slot::Background, &panel_uniforms, (0.0, 0.0)),
            shader(
                Slot::Highlight,
                &highlight_uniforms,
                selected.map_or((0.0, 0.0), |rect| (rect.left, rect.top)),
            ),
            shader(
                Slot::Menu,
                &menu_uniforms,
                menu.map_or((0.0, 0.0), |(mx, my)| (mx, my)),
            ),
        )
    } else {
        (None, None, None)
    };
    ctx.fx.first_frame = false;
    let frame = Frame {
        spec: &spec,
        palette: &palette,
        metrics: &metrics,
        font: ctx.font.as_ref(),
        opacity,
        list,
        hover: ctx.fx.hover,
        pager: ctx.fx.pager,
        menu,
        menu_hover: ctx.fx.menu_hover,
        effects: panel::Effects {
            background: background.as_ref(),
            highlight: highlight.as_ref(),
            menu: menu_effect.as_ref(),
        },
    };
    let started = Instant::now();
    let drawn = ctx.pipe.draw(frame);
    if allowed {
        ctx.budget.observe(started.elapsed(), Instant::now());
    }
    if let Err(e) = drawn {
        println!("Skia 出帧失败,丢帧: {e}");
        return;
    }
    if let Err(e) = ctx.pipe.present() {
        println!("Skia 呈现失败: {e}");
    }
}

/// 鼠标/菜单变化后的重绘;淡出窗口不接鼠标,内容与位置都不动。
pub(super) fn redraw(ctx: &mut WinCtx) {
    if ctx.fx.fading() {
        return;
    }
    let Some(view) = ctx.fx.last.clone() else {
        return;
    };
    draw_view(ctx, &view);
}

/// 菜单锚点:锚在行下缘,贴近面板底部时翻到行上方;与 GPUI 路径同款规则。
/// 行位置取自 `item_rects`,横排换行时也不会锚错行。
pub(super) fn menu_origin(
    slot: usize,
    spec: &PanelSpec,
    metrics: &Metrics,
    font: Option<&Font>,
) -> (f32, f32) {
    let s = metrics.scale();
    let panel_h = panel::panel_height(spec, metrics, font);
    let menu_h = panel::menu_height(metrics);
    let rects = panel::item_rects(spec, metrics, font);
    let Some(rect) = rects.get(slot).copied() else {
        return (8.0 * s, panel::PREEDIT_H * s);
    };
    let top = if rect.bottom + menu_h <= panel_h {
        rect.bottom
    } else {
        (rect.top - menu_h).max(panel::PREEDIT_H * s)
    };
    (8.0 * s, top)
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ctx = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WinCtx;
    if ctx.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    match msg {
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MOUSEWHEEL | WM_SETCURSOR => unsafe {
            ui_skia_mouse::on_mouse(&mut *ctx, hwnd, msg, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 淡出期间点击穿透到宿主;实时组合才接鼠标。与 GPUI 路径同款。
fn style_click_through(hwnd: HWND, click_through: bool) {
    let old = ex_style(hwnd);
    let mut style = old;
    if click_through {
        style |= WS_EX_TRANSPARENT.0;
    } else {
        style &= !WS_EX_TRANSPARENT.0;
    }
    if style != old {
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
        }
    }
}

/// 窗口的扩展样式。
fn ex_style(hwnd: HWND) -> u32 {
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 }
}

/// 系统「减少动态效果」;开启后动效全部走终态。查询失败按可用处理。
pub(super) fn reduce_motion() -> bool {
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

/// 0..1 的动画进度;已过期钳到 1。
fn progress(total: Duration, end: Instant, now: Instant) -> f32 {
    let remain = end.checked_duration_since(now).unwrap_or_default();
    (1.0 - remain.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

/// GPUI 同款 ease_out_quint,两条后端的动效曲线一致。
fn ease_out_quint(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(5)
}

/// DirectWrite 默认字体管理器里取微软雅黑;系统里没有这一族就退回系统默认
/// 字型——拿不到字型就没有字,候选窗会空成一块底色,比字体难看得多。
pub(super) fn panel_font() -> Option<Font> {
    lueying_server::font::panel_font()
}

/// 无焦点置顶工具窗;样式在创建时给足,停靠点出生,避免 Alt+Tab 里露脸。
pub(super) fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let class = w!("lueying-skia-candidate");
        let (park_x, park_y) = ui_place::park_point();
        let hwnd = CreateWindowExW(
            // NOREDIRECTIONBITMAP:窗口自身不画,画面由 DComp 视觉树给出。
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
            class,
            w!("lueying"),
            WS_POPUP | WS_VISIBLE,
            park_x,
            park_y,
            360,
            76,
            None,
            None,
            Some(HINSTANCE(hmodule.0)),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        Ok(hwnd)
    }
}

/// CPU 光栅兜底用的分层窗口:画面由 UpdateLayeredWindow 给,窗口自己不画,
/// 所以不能带 NOREDIRECTIONBITMAP。
pub(super) fn create_layered_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let (park_x, park_y) = ui_place::park_point();
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("lueying-skia-candidate"),
            w!("lueying"),
            WS_POPUP | WS_VISIBLE,
            park_x,
            park_y,
            360,
            76,
            None,
            None,
            Some(HINSTANCE(hmodule.0)),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        Ok(hwnd)
    }
}

/// 窗口类进程内只注册一次(自检与候选窗线程可能先后建窗)。
fn ensure_class(hinstance: HINSTANCE) -> Result<(), String> {
    static ONCE: Once = Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            hCursor: match LoadCursorW(None, IDC_ARROW) {
                Ok(cursor) => cursor,
                Err(e) => {
                    result = Err(format!("LoadCursorW: {e}"));
                    return;
                }
            },
            lpszClassName: w!("lueying-skia-candidate"),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW 失败".into());
        }
    });
    result
}

/// 挪到虚拟桌面之外并保持尺寸;停靠点按当前虚拟桌面算。
fn park(ctx: &mut WinCtx) {
    let (x, y) = ui_place::park_point();
    ui_place::park(ctx.hwnd, x, y);
}
