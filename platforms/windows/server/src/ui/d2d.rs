//! D2D 候选窗:Win32 无焦点弹出窗 + DirectComposition 合成交换链 + Direct2D。
//!
//! 候选窗:独立线程自建消息泵,等版本号事件、窗口消息与动画帧;摆位、停靠、DPI 与
//! 全屏避让同 `ui::place` 助手共用。交互 parity:淡入/淡出、翻页滑动、点选/
//! 滚轮/翻页箭头、右键菜单与悬停,动效可被按键打断,「减少动态效果」时
//! 全部走终态。管线在 `ui::d2d::pipe`:硬件优先,没有适配器退 WARP;设备
//! 被系统摘掉就地整条重建,窗口不换。`LUEYING_D2D_SELFTEST` 供 CI 自检整条
//! 管线。模式徽标走原生分层窗口(`ui::hud`),不在这条管线上。

use std::sync::atomic::Ordering;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use lueying_extproto::candidate::CandidateState;
use lueying_extwasm::{SceneIn, ScenePatch};
use lueying_protocol::SessionView;
use lueying_render::Spring;
use lueying_render::panel::{self, Metrics, PanelSpec};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE, GWLP_USERDATA,
    GetWindowLongPtrW, HCURSOR, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LoadCursorW, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
    RegisterClassW, SPI_GETCLIENTAREAANIMATION, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SystemParametersInfoW, TranslateMessage, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_SETCURSOR, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{BOOL, w};

mod material;
mod mouse;
mod pipe;
pub(crate) mod selftest;

use crate::imp::State;
use crate::ui::d2d::material::Material;
use crate::ui::d2d::pipe::{Frame, Pipeline, Target, Text};
use crate::ui::place::{self, CARET_GAP};

/// 淡入/淡出期间的重绘节奏。
const ANIM_TICK: Duration = Duration::from_millis(5);
/// 候选文字最大宽度(逻辑像素):窗口宽度减去页码与翻页箭头的空间。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 出现动画的起始落差(逻辑像素);乘缩放成物理像素,弹簧把它带回原位。
const ENTER_DISTANCE: f32 = 6.0;
/// 出现弹簧参数:刚度、阻尼、质量;与 macOS 候选窗同一组。
const SPRING_STIFFNESS: f32 = 360.0;
const SPRING_DAMPING: f32 = 28.0;
const SPRING_MASS: f32 = 1.0;
/// 组合结束后的淡出时长;期间沿用最后一份内容并让鼠标穿透。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 翻页时候选内容横向滑入,90ms 内归位。
const PAGE_SLIDE: Duration = Duration::from_millis(90);
/// 停顿重排换了首选时,候选列表轻微变暗再亮起的时长。
const PROMOTE_FLASH: Duration = Duration::from_millis(160);

/// 启动 D2D 候选窗线程;建窗或建管线失败只写日志,输入法照常工作。
pub(super) fn spawn(state: Arc<State>) {
    let spawned = std::thread::Builder::new()
        .name("d2d-candidate".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| window_loop(state)))
                .is_err()
            {
                println!("D2D 候选窗线程崩溃,后续组合不再有候选窗");
            }
        });
    if let Err(e) = spawned {
        println!("D2D 候选窗线程启动失败: {e}");
    }
}

/// 窗口线程的动画与交互状态;全部归窗口线程所有。
#[derive(Default)]
pub(super) struct Fx {
    /// 出现弹簧:窗口从落点下方弹回,落定清空。
    show: Option<Spring>,
    /// 本次出现的起始落差(物理像素);进度归一用。
    enter: f32,
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
    /// 上一次看到的换首选次数;变化即触发一次提示。
    promotions: u32,
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
    /// 本次组合的起点;出现动画与外观扩展的时钟从这儿算。
    since: Option<Instant>,
    /// 静止位置(屏幕坐标);弹簧只在它附近挪窗口。
    rest: (i32, i32, i32, i32),
}

impl Fx {
    /// 回到空闲态;reduce 偏好保留。
    fn clear(&mut self) {
        *self = Fx {
            reduce: self.reduce,
            // 次数跨组合保留,重新出现时不被当成一次新的换首选。
            promotions: self.promotions,
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

    /// 入场/翻页进度:静止为 1,淡出时退回 0;出现中按弹簧位移归一。
    fn anim(&self, now: Instant) -> f32 {
        if let Some(spring) = self.show {
            return (1.0 - spring.value() / self.enter.max(1.0)).clamp(0.0, 1.0);
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
    /// 上一次发布给原生扩展的候选窗状态;内容没变就不再发。
    sent: Option<CandidateState>,
    /// 候选窗窗口。
    pub(super) hwnd: HWND,
    pub(super) state: Arc<State>,
    pub(super) fx: Fx,
    /// 出帧管线;设备被摘掉时整个换成新的,所以是 Option。
    pipe: Option<Pipeline>,
    /// DirectWrite 文字引擎;布局与绘制共用同一份测宽。
    pub(super) text: Text,
    pub(super) cursor_arrow: Option<HCURSOR>,
    pub(super) cursor_hand: Option<HCURSOR>,
    /// 运行期扩展:后台 worker 跑 Extism 插件,这里只收主题色补丁。
    script: crate::ext_host::Script,
    script_accent: Option<u32>,
    /// 隐私字段(密码框):外观扩展开关与补丁停用。
    private: bool,
    /// 首帧真正呈现前窗口保持隐藏,避免从停靠点挪到光标处时闪一帧旧内容。
    visible: bool,
    /// 出帧路径彻底走不通:候选窗停用。
    dead: bool,
    /// 已落的背板材质:(期望态, 亚克力是否启用);变化才调窗口属性。
    material: Option<(Material, bool)>,
}

/// 窗口线程主循环:排干消息、轮询会话、按需重绘。
fn window_loop(state: Arc<State>) {
    // DComp 与窗口消息都归这条线程;COM 已按别的 apartment 初始化过时忽略。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            println!("D2D 候选窗建窗失败: {e}");
            return;
        }
    };
    let pipe = match Pipeline::new(hwnd) {
        Ok(pipe) => Some(pipe),
        Err(e) => {
            println!("D2D 候选窗管线不可用: {e};候选窗停用");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            None
        }
    };
    let text = match Text::new() {
        Ok(text) => text,
        Err(e) => {
            println!("D2D 文字引擎起不来: {e}");
            return;
        }
    };
    let (cursor_arrow, cursor_hand) = unsafe {
        (
            LoadCursorW(None, IDC_ARROW).ok(),
            LoadCursorW(None, IDC_HAND).ok(),
        )
    };
    let script = crate::ext_host::Script::spawn(Arc::clone(&state));
    // 窗口过程要反查状态;循环常驻,泄漏的 Box 与进程同寿命。
    let dead = pipe.is_none();
    let ctx: *mut WinCtx = Box::leak(Box::new(WinCtx {
        sent: None,
        hwnd,
        state: Arc::clone(&state),
        fx: Fx::default(),
        pipe,
        text,
        cursor_arrow,
        cursor_hand,
        script,
        script_accent: None,
        private: false,
        visible: false,
        dead,
        material: None,
    }));
    unsafe {
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);
    }
    let mut last_version = 0u64;
    loop {
        // 空闲时一直等下去:版本号、窗口消息与脚本补丁都会叫醒这里;
        // 动画进行时按帧推进,其余时间不烧 CPU。
        let waiting = busy_animating(unsafe { &*ctx });
        let timeout = waiting.then_some(ANIM_TICK);
        wait_for_input(&state, timeout);
        // 无焦点窗口的消息量很小,醒来顺手排干。
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // 脚本补丁到了就应用;这一轮有新内容时由 place 一起重绘。
        let patch = unsafe { &(*ctx).script }.take();
        if let Some((seq, patch)) = patch {
            let ctx = unsafe { &mut *ctx };
            let redraw_now = state.version.load(Ordering::Acquire) == last_version;
            apply_script(ctx, seq, patch, redraw_now);
        }
        let version = state.version.load(Ordering::Acquire);
        if version != last_version {
            last_version = version;
            // 消息泵之外取可变借用;窗口过程里的借用都已结束。
            let ctx = unsafe { &mut *ctx };
            ctx.fx.reduce = reduce_motion();
            place(ctx);
        }
        // 淡入/淡出/滑动进行中按循环节奏重绘。
        let ctx = unsafe { &mut *ctx };
        if busy_animating(ctx) {
            animate(ctx);
        }
    }
}

/// 淡入/淡出/滑动是否还在推进。
fn busy_animating(ctx: &WinCtx) -> bool {
    let fx = &ctx.fx;
    fx.hide.is_some() || fx.show.is_some() || fx.slide.is_some() || fx.flash.is_some()
}

/// 等版本号事件、窗口消息或动画帧;`timeout` 为空时一直睡到有事件。
fn wait_for_input(state: &State, timeout: Option<Duration>) {
    let event = state.wake_ui.raw();
    if event.is_invalid() {
        std::thread::sleep(timeout.unwrap_or(Duration::from_millis(15)));
        return;
    }
    let ms = timeout.map_or(windows::Win32::System::Threading::INFINITE, |wait| {
        wait.as_millis().min(u128::from(u32::MAX)) as u32
    });
    unsafe {
        MsgWaitForMultipleObjectsEx(Some(&[event]), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
    }
}

/// 组合变化后重新摆位并出帧;无内容时淡出或停靠。
fn place(ctx: &mut WinCtx) {
    if ctx.dead {
        // 出帧路径没了,窗口一直停在屏幕外,不再参与摆位与绘制。
        park(ctx);
        return;
    }
    match place::snapshot(&ctx.state) {
        None => {
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
            publish_state(ctx, None);
        }
        Some(snap) => {
            ctx.private = snap.private;
            let view = &snap.view;
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
                // 新组合:出现动画与外观扩展的时钟从现在起算。
                ctx.fx.since = Some(Instant::now());
                // 出现动画:窗口从落点下方 6pt 弹回原位;「减少动态效果」直接就位。
                ctx.fx.enter = match view.caret {
                    Some((l, _, r, b)) => ENTER_DISTANCE * place::scale_at(l.min(r), b),
                    None => ENTER_DISTANCE,
                };
                ctx.fx.show = if ctx.fx.reduce || !effects_enabled(&ctx.state) {
                    None
                } else {
                    let mut spring = Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_MASS);
                    spring.set(ctx.fx.enter);
                    Some(spring)
                };
            }
            ctx.fx.hide = None;
            style_click_through(ctx.hwnd, false);
            // 只有翻页才滑动;首次展开和继续打字不横向跳。
            if !ctx.fx.reduce
                && effects_enabled(&ctx.state)
                && ctx.fx.page.is_some_and(|page| page != view.page)
            {
                let previous = ctx.fx.page.unwrap_or(view.page);
                let direction = if view.page > previous { 1.0 } else { -1.0 };
                ctx.fx.slide = Some((direction, Instant::now() + PAGE_SLIDE));
                ctx.fx.hover = None;
                ctx.fx.pager = None;
            }
            ctx.fx.page = Some(view.page);
            if view.promotions != ctx.fx.promotions {
                ctx.fx.promotions = view.promotions;
                if !ctx.fx.reduce && effects_enabled(&ctx.state) {
                    ctx.fx.flash = Some(Instant::now() + PROMOTE_FLASH);
                }
            }
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
            ctx.fx.rest = rect;
            let (x, y, w, h) = rect;
            // 出现动画里窗口带着弹簧位移落位;静止时位移为 0,落点不变。
            let dy = ctx
                .fx
                .show
                .as_ref()
                .map_or(0.0, |spring| f64::from(spring.value()));
            if let Err(e) = layout_window(ctx, (x, y + dy.round() as i32, w, h)) {
                println!("D2D 候选窗调整失败: {e}");
                return;
            }
            // 先落内容再出帧;淡出期间用的就是这份冻结内容。
            ctx.fx.last = Some(view.clone());
            submit_scene(ctx, view, snap.private);
            draw_view(ctx, view);
            publish_state(ctx, Some(view));
        }
    }
}

/// 把当前面板状态交给脚本线程;补丁晚一帧到,不影响这一帧。
fn submit_scene(ctx: &WinCtx, view: &SessionView, private: bool) {
    let now = Instant::now();
    let (_, _, accent) = place::ui_options(&ctx.state);
    let appearance = lueying_server::theme::Appearance::of_system();
    let colors = lueying_server::theme::colors_with(appearance, accent);
    let page_size = view.page_size.max(1);
    let start = view.page * page_size;
    let candidates = view.candidates.len().saturating_sub(start).min(page_size);
    let selected = if candidates == 0 {
        0
    } else {
        view.highlight.saturating_sub(start).min(candidates - 1)
    };
    let scene = SceneIn {
        composing: true,
        page: (view.page + 1) as u32,
        pages: view.candidates.len().div_ceil(page_size).max(1) as u32,
        candidates: candidates as u32,
        selected: selected as u32,
        accent: colors.accent,
        dark: matches!(appearance, lueying_server::theme::Appearance::Dark),
        high_contrast: matches!(appearance, lueying_server::theme::Appearance::HighContrast),
        reduce_motion: ctx.fx.reduce,
        time: ctx.fx.clock(now),
        anim: ctx.fx.anim(now),
        private,
        ..SceneIn::default()
    };
    ctx.script.submit(scene);
}

/// 应用脚本补丁;只有主题色还有效,内容没变就不重绘。
fn apply_script(ctx: &mut WinCtx, seq: u64, patch: Option<ScenePatch>, redraw_now: bool) {
    // 旧外观补丁晚到:新一帧或隐私字段已经提交,直接丢弃。
    if seq < ctx.script.seq() {
        return;
    }
    let accent = patch.and_then(|patch| patch.accent);
    if accent == ctx.script_accent {
        return;
    }
    ctx.script_accent = accent;
    if redraw_now {
        redraw(ctx);
    }
}

/// 光标位置算出的候选窗矩形(x, y, 宽, 高);宽为 0 表示这次不弹。
fn caret_rect(ctx: &mut WinCtx, view: &SessionView) -> (i32, i32, i32, i32) {
    let (layout, ui_font, _) = place::ui_options(&ctx.state);
    let host_pid = (ctx.state.active.load(Ordering::Relaxed) >> 32) as u32;
    let Some((l, t, r, b)) = view.caret else {
        ctx.fx.above = None;
        ctx.state.candidate_above.store(false, Ordering::Relaxed);
        return (0, 0, 0, 0);
    };
    let left = l.min(r);
    // 按光标所在显示器算 DPI;停靠屏的缩放与本屏无关。
    let scale = place::scale_at(left, b);
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let mut spec = place::spec_of(view, layout);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = place::measure_width(view, layout, &metrics, &ctx.text);
    let mut w = panel::panel_width(&spec, &metrics, &ctx.text) as i32;
    let h = panel::panel_height(&spec, &metrics, &ctx.text) as i32;
    let work = place::work_area_at(left, b);
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
    let hidden = place::foreground_fullscreen_at(left, b, host_pid);
    let w = if hidden { 0 } else { w };
    (x, y, w, h)
}

/// 摆位并把表面尺寸对上。
fn layout_window(ctx: &mut WinCtx, rect: (i32, i32, i32, i32)) -> Result<(), String> {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return Err("候选窗尺寸为空".to_string());
    }
    unsafe {
        let _ = SetWindowPos(ctx.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    }
    let Some(pipe) = ctx.pipe.as_mut() else {
        return Err("出帧管线不可用".to_string());
    };
    pipe.rebuild_surfaces(w as u32, h as u32)
}

/// 动画帧:淡入/淡出/滑动/弹性动效推进中重绘;淡出到期停靠,其余到期走终态。
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
    if ctx.fx.slide.is_some_and(|(_, end)| now >= end) {
        ctx.fx.slide = None;
    }
    if ctx.fx.flash.is_some_and(|end| now >= end) {
        ctx.fx.flash = None;
    }
    // 出现弹簧推进一帧;窗口跟着弹簧值落位,落定精确停在静止位置。
    let mut settled = false;
    if let Some(spring) = ctx.fx.show.as_mut() {
        if spring.step(ANIM_TICK.as_secs_f32()) {
            let (x, y, w, h) = ctx.fx.rest;
            let dy = f64::from(spring.value());
            unsafe {
                let _ = SetWindowPos(
                    ctx.hwnd,
                    Some(HWND_TOPMOST),
                    x,
                    y + dy.round() as i32,
                    w,
                    h,
                    SWP_NOACTIVATE,
                );
            }
        } else {
            settled = true;
        }
    }
    if settled {
        ctx.fx.show = None;
        let (x, y, w, h) = ctx.fx.rest;
        unsafe {
            let _ = SetWindowPos(ctx.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
        }
    }
    let animating = ctx.fx.hide.is_some() || ctx.fx.show.is_some() || ctx.fx.slide.is_some();
    if let Some(view) = animating.then(|| ctx.fx.last.clone()).flatten() {
        draw_view(ctx, &view);
    }
}

/// 出一帧:透明度(淡出 > 淡入)、列表动效、悬停与菜单,全部换算后交给管线。
fn draw_view(ctx: &mut WinCtx, view: &SessionView) {
    if ctx.dead {
        return;
    }
    // 设备被系统摘掉(驱动重启、GPU 挂起)后整条管线重建,窗口不用换。
    if let Some(pipe) = ctx.pipe.as_ref()
        && pipe.device_removed()
    {
        println!("D2D 候选窗:设备被摘掉,就地重建管线");
        ctx.pipe = None;
        match Pipeline::new(ctx.hwnd) {
            Ok(pipe) => {
                ctx.pipe = Some(pipe);
                ctx.visible = false;
                // 新表面尺寸跟窗口走,重摆一次把表面上对。
                let rect = caret_rect(ctx, view);
                if let Err(e) = layout_window(ctx, rect) {
                    println!("D2D 候选窗调整失败: {e}");
                    return;
                }
            }
            Err(e) => {
                ctx.dead = true;
                println!("D2D 候选窗:管线重建失败({e}),候选窗停用");
                return;
            }
        }
    }
    let (layout, ui_font, accent) = place::ui_options(&ctx.state);
    let appearance = lueying_server::theme::Appearance::of_system();
    // 高对比度由系统接管外观,外观扩展整个关掉,免得压过系统配色。
    let high_contrast = matches!(appearance, lueying_server::theme::Appearance::HighContrast);
    // 脚本主题色只换主色与选中底色;高对比度仍用系统色。
    let colors = match ctx.script_accent {
        Some(rgb) if !high_contrast => lueying_server::theme::colors_with_accent(appearance, rgb),
        _ => lueying_server::theme::colors_with(appearance, accent),
    };
    // 皮肤(skin.toml)叠在脚本主题色之上,是用户显式放置的最后一层;
    // 高对比度下 with_skin 原样返回。
    let skin = ctx.state.skin.lock().unwrap_or_else(|p| p.into_inner());
    // 用户的 skin.toml 是显式选择的自绘配色,有皮肤就不上材质背板。
    let skinless = skin.is_empty();
    let palette =
        lueying_server::theme::palette(lueying_server::theme::with_skin(colors, appearance, &skin));
    // 皮肤只参与配色;早点放锁,后面的出帧与显示不再借 `ctx`。
    drop(skin);
    // 材质背板:毛玻璃由 DWM 出底色,自绘只铺半透明内容层;系统关闭
    // 「透明效果」、高对比度、有皮肤或落属性失败时回实底主题。
    let wanted = Material {
        on: !high_contrast && skinless && material::transparency_enabled(),
        tint: colors.bg,
        dark: matches!(appearance, lueying_server::theme::Appearance::Dark),
    };
    let glass = match ctx.material {
        Some((cached, applied)) if cached == wanted => applied,
        _ => {
            let applied = material::apply(ctx.hwnd, wanted);
            ctx.material = Some((wanted, applied));
            applied
        }
    };
    // 摆位时已把窗口放到光标屏;动效重绘沿用同一份缩放。
    let scale = match view.caret {
        Some((l, _, r, b)) => place::scale_at(l.min(r), b),
        None => 1.0,
    };
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let mut spec = place::spec_of(view, layout);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = place::measure_width(view, layout, &metrics, &ctx.text);
    let now = Instant::now();
    // 出现靠弹簧位移,透明度恒为 1;淡出仍按旧节奏渐隐。
    let opacity = if let Some(end) = ctx.fx.hide {
        (1.0 - ease_out_quint(progress(HIDE_FADE, end, now))).max(0.0)
    } else {
        1.0
    };
    let list = match (ctx.fx.slide, ctx.fx.flash) {
        (Some((direction, end)), _) if now < end => {
            let delta = ease_out_quint(progress(PAGE_SLIDE, end, now));
            panel::ListFx {
                dx: direction * 10.0 * metrics.scale() * (1.0 - delta),
                opacity: 0.55 + 0.45 * delta,
            }
        }
        (_, Some(end)) if now < end => {
            // 换首选:列表只轻微变暗再亮起,不横向移动。
            let delta = ease_out_quint(progress(PROMOTE_FLASH, end, now));
            panel::ListFx {
                dx: 0.0,
                opacity: 0.72 + 0.28 * delta,
            }
        }
        _ => panel::ListFx::NEUTRAL,
    };
    // 菜单只挂在内容未变的行上;翻页或继续打字后行文本对不上,菜单自动失效。
    let menu = ctx.fx.menu.as_ref().and_then(|(slot, _, text)| {
        spec.items
            .get(*slot)
            .is_some_and(|item| &item.text == text)
            .then(|| menu_origin(*slot, &spec, &metrics, &ctx.text))
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
    let frame = Frame {
        spec: &spec,
        palette: &palette,
        metrics,
        glass,
        opacity,
        list,
        hover: ctx.fx.hover,
        pager: ctx.fx.pager,
        menu,
        menu_hover: ctx.fx.menu_hover,
    };
    let drawn = match ctx.pipe.as_mut() {
        Some(pipe) => pipe
            .paint_frame(&frame, &ctx.text)
            .and_then(|()| pipe.present()),
        None => Err("出帧管线不可用".to_string()),
    };
    if let Err(e) = drawn {
        println!("D2D 出帧失败,丢帧: {e}");
        return;
    }
    show(ctx);
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

/// 菜单锚点:锚在行下缘,贴近面板底部时翻到行上方。
/// 行位置取自 `item_rects`,横排换行时也不会锚错行。
pub(super) fn menu_origin(
    slot: usize,
    spec: &PanelSpec,
    metrics: &Metrics,
    measure: &dyn panel::Measure,
) -> (f32, f32) {
    let s = metrics.scale();
    let panel_h = panel::panel_height(spec, metrics, measure);
    let menu_h = panel::menu_height(metrics);
    let rects = panel::item_rects(spec, metrics, measure);
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
            mouse::on_mouse(&mut *ctx, hwnd, msg, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 淡出期间点击穿透到宿主;实时组合才接鼠标。
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

/// 弹性动效开关;配置重载后立即生效。
fn effects_enabled(state: &State) -> bool {
    state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .effects
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

/// ease_out_quint;窗口滑动与淡出动效共用这条曲线。
fn ease_out_quint(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(5)
}

/// 无焦点置顶工具窗;样式在创建时给足,停靠点出生,避免 Alt+Tab 里露脸。
pub(super) fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let class = w!("lueying-d2d-candidate");
        let (park_x, park_y) = place::park_point();
        let hwnd = CreateWindowExW(
            // NOREDIRECTIONBITMAP:窗口自身不画,画面由 DComp 视觉树给出。
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
            class,
            w!("lueying"),
            // 先隐藏建在停靠点;首帧真正提交后再显示,避免闪一帧旧内容。
            WS_POPUP,
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
            lpszClassName: w!("lueying-d2d-candidate"),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW 失败".into());
        }
    });
    result
}

/// 首帧真正提交后再显示;无焦点显示,不抢宿主焦点。
fn show(ctx: &mut WinCtx) {
    if ctx.visible {
        return;
    }
    unsafe {
        let _ = ShowWindow(ctx.hwnd, SW_SHOWNOACTIVATE);
    }
    ctx.visible = true;
}

/// 挪到虚拟桌面之外并隐藏;停靠点按当前虚拟桌面算。
fn park(ctx: &mut WinCtx) {
    if ctx.visible {
        unsafe {
            let _ = ShowWindow(ctx.hwnd, SW_HIDE);
        }
        ctx.visible = false;
    }
    let (x, y) = place::park_point();
    place::park(ctx.hwnd, x, y);
    publish_state(ctx, None);
}

/// 把候选窗的当前状态发布给订阅的原生扩展;内容没变就不发。
/// 私密字段的文字在这里就留空,扩展拿不到。
fn publish_state(ctx: &mut WinCtx, view: Option<&SessionView>) {
    let (x, y, w, h) = ctx.fx.rest;
    let private = ctx.private;
    let mut state = CandidateState {
        visible: ctx.visible,
        composing: view.is_some(),
        rect: [x, y, w, h],
        private,
        ..CandidateState::default()
    };
    if let Some(view) = view.filter(|_| !private) {
        state.raw.clone_from(&view.raw);
        state.page = view.page;
        state.page_size = view.page_size;
        state.highlight = view.highlight;
        state.candidates.clone_from(&view.candidates);
    }
    if ctx.sent.as_ref() == Some(&state) {
        return;
    }
    crate::candidate_events::publish(state.clone());
    ctx.sent = Some(state);
}
