//! Skia 候选窗:Win32 无焦点弹出窗 + DirectComposition 交换链 + Ganesh-D3D12。
//!
//! `ui_backend=skia` 时启用(默认仍是 GPUI):独立线程自建消息泵,15ms
//! 轮询会话版本号;摆位、停靠、DPI 与全屏避让同 GPUI 路径共用 `ui_place`。
//! 交互 parity:淡入/淡出、翻页滑动、点选/滚轮/翻页箭头、右键菜单与悬停,
//! 动效可被按键打断,「减少动态效果」时全部走终态。D3D12 优先挑硬件适配器,
//! 没有就退 WARP,建不出设备时放弃候选窗并写日志,输入本身不受影响。
//! `LUEYING_SKIA_SELFTEST` 供 CI 自检整条管线。模式徽标仍是 GPUI(Stage 2b)。

use std::sync::atomic::Ordering;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use lueying_protocol::{Request, SessionView};
use lueying_render::panel::{self, Item, Layout, Metrics, PanelSpec, Palette, PreeditBar};
use lueying_server::config::UiLayout;
use lueying_server::theme;
use skia_safe::{Font, FontStyle, FontMgr};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE, GWLP_USERDATA,
    GetWindowLongPtrW, HCURSOR, HWND_TOPMOST, IDC_ARROW, IDC_HAND, LoadCursorW, MSG, PM_REMOVE,
    PeekMessageW, RegisterClassW, SPI_GETCLIENTAREAANIMATION, SWP_NOACTIVATE,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetCursor, SetWindowLongPtrW, SetWindowPos,
    SystemParametersInfoW, TranslateMessage, WNDCLASSW, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_SETCURSOR, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};
use windows::core::{BOOL, w};

use crate::imp::State;
use crate::ui_place::{self, CARET_GAP};
use crate::ui_skia_gpu::{Frame, FrameTarget, Offscreen, Pipeline, rgb};

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

/// 启动 Skia 候选窗线程;建窗或建管线失败只写日志,输入法照常工作。
pub(super) fn spawn(state: Arc<State>) {
    let spawned = std::thread::Builder::new()
        .name("skia-candidate".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| window_loop(state))).is_err()
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
struct Fx {
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
    hover: Option<usize>,
    /// 悬停的翻页箭头;单页时恒为 `None`。
    pager: Option<panel::Hit>,
    /// 打开的右键菜单:(页内槽位, 菜单顶部 y, 行文本)。行文本变了菜单自动失效。
    menu: Option<(usize, f32, String)>,
    /// 悬停的菜单项。
    menu_hover: Option<usize>,
    /// 鼠标最后位置(客户区);WM_SETCURSOR 用它做命中,省一次坐标换算。
    mouse: (f32, f32),
    /// 最近一帧的绘制输入;命中测试用它,保证所指即所画。
    geometry: Option<Geom>,
}

impl Fx {
    /// 回到空闲态;reduce 偏好保留。
    fn clear(&mut self) {
        *self = Fx {
            reduce: self.reduce,
            ..Fx::default()
        };
    }
}

/// 最近一帧的绘制输入快照;鼠标命中据此换算,不重算布局。
struct Geom {
    spec: PanelSpec,
    metrics: Metrics,
    menu_xy: Option<(f32, f32)>,
}

/// 窗口过程持有的上下文;窗口与线程同生命周期,进程退出一起回收。
struct WinCtx {
    state: Arc<State>,
    fx: Fx,
    pipe: Pipeline,
    font: Option<Font>,
    cursor_arrow: Option<HCURSOR>,
    cursor_hand: Option<HCURSOR>,
}

fn window_loop(state: Arc<State>) {
    // DComp 与窗口消息都归这条线程;COM 已按别的 apartment 初始化过时忽略。
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            println!("Skia 候选窗建窗失败: {e}");
            return;
        }
    };
    let pipe = match Pipeline::new(hwnd) {
        Ok(pipe) => pipe,
        Err(e) => {
            println!("Skia 候选窗 D3D12 管线初始化失败: {e}");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        }
    };
    println!("Skia 候选窗就绪:Ganesh-D3D12 + DirectComposition");
    let font = panel_font();
    let (cursor_arrow, cursor_hand) = unsafe {
        (
            LoadCursorW(None, IDC_ARROW).ok(),
            LoadCursorW(None, IDC_HAND).ok(),
        )
    };
    // 窗口过程要反查状态;循环常驻,泄漏的 Box 与进程同寿命。
    let ctx: *mut WinCtx = Box::leak(Box::new(WinCtx {
        state: Arc::clone(&state),
        fx: Fx::default(),
        pipe,
        font,
        cursor_arrow,
        cursor_hand,
    }));
    unsafe {
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);
    }
    let mut last_version = 0u64;
    let mut next_poll = Instant::now();
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
        if now >= next_poll {
            next_poll = now + POLL;
            let version = state.version.load(Ordering::Relaxed);
            if version != last_version {
                last_version = version;
                // 消息泵之外取可变借用;窗口过程里的借用都已结束。
                let ctx = unsafe { &mut *ctx };
                ctx.fx.reduce = reduce_motion();
                place(ctx, hwnd);
            }
        }
        // 淡入/淡出/滑动进行中按循环节奏重绘;5ms 粒度足够 90–140ms 的动效。
        let animating = unsafe {
            let fx = &(*ctx).fx;
            fx.hide.is_some() || fx.show.is_some() || fx.slide.is_some()
        };
        if animating {
            let ctx = unsafe { &mut *ctx };
            animate(ctx, hwnd);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 组合变化后重新摆位并出帧;无内容时淡出或停靠。
fn place(ctx: &mut WinCtx, hwnd: HWND) {
    let Some(view) = ui_place::snapshot(&ctx.state) else {
        // 组合结束:reduce 直接停靠;否则窗口留在原地淡出,到期由动画帧停靠。
        if ctx.fx.reduce || ctx.fx.last.is_none() {
            ctx.fx.clear();
            park(hwnd);
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
            style_click_through(hwnd, true);
        }
        return;
    };
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
        ctx.fx.show = if ctx.fx.reduce {
            None
        } else {
            Some(Instant::now() + SHOW_FADE)
        };
    }
    ctx.fx.hide = None;
    style_click_through(hwnd, false);
    // 只有翻页才滑动;首次展开和继续打字不横向跳。
    if !ctx.fx.reduce && ctx.fx.page.is_some_and(|page| page != view.page) {
        let previous = ctx.fx.page.unwrap_or(view.page);
        let direction = if view.page > previous { 1.0 } else { -1.0 };
        ctx.fx.slide = Some((direction, Instant::now() + PAGE_SLIDE));
        ctx.fx.hover = None;
        ctx.fx.pager = None;
    }
    ctx.fx.page = Some(view.page);
    let (layout, ui_font, _) = ui_place::ui_options(&ctx.state);
    let host_pid = (ctx.state.active.load(Ordering::Relaxed) >> 32) as u32;
    let (x, y, w, h) = match view.caret {
        Some((l, t, r, b)) => {
            let left = l.min(r);
            // 按光标所在显示器算 DPI;停靠屏的缩放与本屏无关。
            let scale = ui_place::scale_at(left, b);
            let metrics = Metrics::scaled(ui_font.factor() * scale);
            let mut spec = spec_of(&view, layout);
            spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
            spec.width = measure_width(&view, layout, &metrics, ctx.font.as_ref());
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
            let y = (if prefer_above { over } else { below })
                .clamp(work.top, (work.bottom - h).max(work.top));
            let x = left.clamp(work.left, (work.right - w).max(work.left));
            // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
            let hidden = ui_place::foreground_fullscreen_at(left, b, host_pid);
            let w = if hidden { 0 } else { w };
            (x, y, w, h)
        }
        None => {
            ctx.fx.above = None;
            (0, 0, 0, 0)
        }
    };
    if w == 0 {
        // 独占全屏或拿不到光标信息:停靠窗口,并清掉冻结内容,
        // 下一次组合按新组合淡入,不会接着上一次的状态。
        ctx.fx.show = None;
        ctx.fx.hide = None;
        ctx.fx.slide = None;
        ctx.fx.last = None;
        park(hwnd);
        return;
    }
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    }
    if let Err(e) = ctx.pipe.rebuild_surfaces(w as u32, h as u32) {
        println!("Skia 交换链调整失败: {e}");
        return;
    }
    // 先落内容再出帧;淡出期间用的就是这份冻结内容。
    ctx.fx.last = Some(view.clone());
    draw_view(ctx, &view);
}

/// 动画帧:淡入/淡出/滑动进行中重绘;淡出到期停靠,其余到期走终态。
fn animate(ctx: &mut WinCtx, hwnd: HWND) {
    let now = Instant::now();
    if ctx.fx.hide.is_some_and(|end| now >= end) {
        ctx.fx.clear();
        style_click_through(hwnd, false);
        park(hwnd);
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
    let (layout, ui_font, accent) = ui_place::ui_options(&ctx.state);
    let palette = palette_of(theme::colors_with(theme::Appearance::of_system(), accent));
    // 摆位时已把窗口放到光标屏;动效重绘沿用同一份缩放。
    let scale = match view.caret {
        Some((l, _, r, b)) => ui_place::scale_at(l.min(r), b),
        None => 1.0,
    };
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let mut spec = spec_of(view, layout);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = measure_width(view, layout, &metrics, ctx.font.as_ref());
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
    };
    if let Err(e) = ctx.pipe.draw(frame) {
        println!("Skia 出帧失败,丢帧: {e}");
        return;
    }
    if let Err(e) = ctx.pipe.present() {
        println!("Skia 呈现失败: {e}");
    }
}

/// 菜单锚点:锚在行下缘,贴近面板底部时翻到行上方;与 GPUI 路径同款规则。
/// 行位置取自 `item_rects`,横排换行时也不会锚错行。
fn menu_origin(slot: usize, spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>) -> (f32, f32) {
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

/// 鼠标/菜单变化后的重绘;淡出窗口不接鼠标,内容与位置都不动。
fn redraw(ctx: &mut WinCtx) {
    if ctx.fx.hide.is_some() {
        return;
    }
    let Some(view) = ctx.fx.last.clone() else {
        return;
    };
    draw_view(ctx, &view);
}

/// 可交互:有激活会话且服务端候选窗在显示(组合中)。
fn interactive(state: &State) -> bool {
    state.active.load(Ordering::Relaxed) != 0 && ui_place::composing(state)
}

/// 鼠标位置是否落在可点区域(候选行/箭头/菜单);WM_SETCURSOR 用。
fn hand_hit(ctx: &WinCtx, x: f32, y: f32) -> bool {
    let Some(geom) = ctx.fx.geometry.as_ref() else {
        return false;
    };
    if !matches!(
        panel::hit_test(&geom.spec, &geom.metrics, ctx.font.as_ref(), x, y),
        panel::Hit::Nothing
    ) {
        return true;
    }
    geom.menu_xy.is_some_and(|(mx, my)| {
        panel::menu_hit(mx, my, &geom.metrics, x, y).is_some()
    })
}

/// 左键命中后的动作;菜单项优先于候选行。
enum Click {
    Menu { item: usize, slot: usize },
    Pick(usize),
    Page(i32),
    Nothing,
}

fn on_mouse(ctx: &mut WinCtx, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let x = (lparam.0 & 0xffff) as u16 as i32 as f32;
    let y = ((lparam.0 >> 16) & 0xffff) as u16 as i32 as f32;
    match msg {
        WM_MOUSEMOVE => {
            ctx.fx.mouse = (x, y);
            if ctx.fx.hide.is_some() {
                return LRESULT(0);
            }
            let (item, pager, menu_hover) = match ctx.fx.geometry.as_ref() {
                Some(geom) => {
                    let menu_hover = geom
                        .menu_xy
                        .and_then(|(mx, my)| panel::menu_hit(mx, my, &geom.metrics, x, y));
                    // 菜单项压住的行不参与悬停,避免菜单下还有一块高亮跟着动。
                    let hit = if menu_hover.is_some() {
                        panel::Hit::Nothing
                    } else {
                        panel::hit_test(&geom.spec, &geom.metrics, ctx.font.as_ref(), x, y)
                    };
                    let item = match hit {
                        panel::Hit::Item(index) => Some(index),
                        _ => None,
                    };
                    let pager = match hit {
                        panel::Hit::PrevPage | panel::Hit::NextPage => Some(hit),
                        _ => None,
                    };
                    (item, pager, menu_hover)
                }
                None => (None, None, None),
            };
            if item != ctx.fx.hover || pager != ctx.fx.pager || menu_hover != ctx.fx.menu_hover {
                ctx.fx.hover = item;
                ctx.fx.pager = pager;
                ctx.fx.menu_hover = menu_hover;
                redraw(ctx);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // 命中候选/箭头/菜单给手型,其余交回系统;坐标用最近的 WM_MOUSEMOVE。
            let (mx, my) = ctx.fx.mouse;
            let hand = hand_hit(ctx, mx, my);
            let cursor = if hand { ctx.cursor_hand } else { ctx.cursor_arrow };
            if let Some(cursor) = cursor {
                unsafe { SetCursor(Some(cursor)) };
            }
            LRESULT(if hand { 1 } else { 0 })
        }
        WM_LBUTTONDOWN => {
            if !interactive(&ctx.state) {
                return LRESULT(0);
            }
            let session = ctx.state.active.load(Ordering::Relaxed);
            let click = match ctx.fx.geometry.as_ref() {
                Some(geom) => {
                    if let Some((mx, my)) = geom.menu_xy
                        && let Some(item) = panel::menu_hit(mx, my, &geom.metrics, x, y)
                    {
                        Click::Menu {
                            item,
                            slot: ctx.fx.menu.as_ref().map_or(0, |(slot, _, _)| *slot),
                        }
                    } else {
                        match panel::hit_test(&geom.spec, &geom.metrics, ctx.font.as_ref(), x, y) {
                            panel::Hit::Item(slot) => Click::Pick(slot),
                            panel::Hit::PrevPage => Click::Page(-1),
                            panel::Hit::NextPage => Click::Page(1),
                            panel::Hit::Nothing => Click::Nothing,
                        }
                    }
                }
                None => Click::Nothing,
            };
            match click {
                Click::Menu { item, slot } => {
                    let request = if item == 0 {
                        Request::Pin { session, slot }
                    } else {
                        Request::Demote { session, slot }
                    };
                    let _ = crate::imp::handle(&ctx.state, request);
                    ctx.fx.menu = None;
                    ctx.fx.menu_hover = None;
                    redraw(ctx);
                }
                Click::Pick(slot) => {
                    let _ = crate::imp::handle(&ctx.state, Request::Pick { session, slot });
                }
                Click::Page(delta) => {
                    let _ = crate::imp::handle(&ctx.state, Request::Page { session, delta });
                }
                Click::Nothing => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            if !interactive(&ctx.state) {
                return LRESULT(0);
            }
            let target = match ctx.fx.geometry.as_ref() {
                Some(geom) => match panel::hit_test(&geom.spec, &geom.metrics, ctx.font.as_ref(), x, y)
                {
                    panel::Hit::Item(slot) => geom.spec.items.get(slot).map(|item| {
                        let (_, top) = menu_origin(slot, &geom.spec, &geom.metrics, ctx.font.as_ref());
                        (slot, top, item.text.clone())
                    }),
                    _ => None,
                },
                None => None,
            };
            if let Some((slot, top, text)) = target {
                ctx.fx.menu = Some((slot, top, text));
                ctx.fx.menu_hover = None;
                redraw(ctx);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // 高 16 位是有符号滚轮增量;向上翻上一页,向下翻下一页。
            let delta = (wparam.0 >> 16) as u16 as i16;
            if delta != 0 && interactive(&ctx.state) {
                let session = ctx.state.active.load(Ordering::Relaxed);
                let _ = crate::imp::handle(
                    &ctx.state,
                    Request::Page {
                        session,
                        delta: if delta > 0 { -1 } else { 1 },
                    },
                );
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctx = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WinCtx;
    if ctx.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    match msg {
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MOUSEWHEEL | WM_SETCURSOR => {
            unsafe { on_mouse(&mut *ctx, hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 淡出期间点击穿透到宿主;实时组合才接鼠标。与 GPUI 路径同款。
fn style_click_through(hwnd: HWND, click_through: bool) {
    unsafe {
        let old = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let mut style = old;
        if click_through {
            style |= WS_EX_TRANSPARENT.0;
        } else {
            style &= !WS_EX_TRANSPARENT.0;
        }
        if style != old {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
        }
    }
}

/// 系统「减少动态效果」;开启后动效全部走终态。查询失败按可用处理。
fn reduce_motion() -> bool {
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

/// DirectWrite 默认字体管理器里取微软雅黑;取不到时走无字体路径(只画背景)。
fn panel_font() -> Option<Font> {
    let face = FontMgr::new().match_family_style("Microsoft YaHei UI", FontStyle::normal())?;
    Some(Font::from_typeface(face, 17.0))
}

/// 无焦点置顶工具窗;样式在创建时给足,停靠点出生,避免 Alt+Tab 里露脸。
fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let class = w!("lueying-skia-candidate");
        let (park_x, park_y) = ui_place::park_point();
        let hwnd = CreateWindowExW(
            // NOREDIRECTIONBITMAP:窗口自身不画,画面由 DComp 视觉树给出。
            WS_EX_NOACTIVATE
                | WS_EX_TOOLWINDOW
                | WS_EX_TOPMOST
                | WS_EX_NOREDIRECTIONBITMAP,
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
fn park(hwnd: HWND) {
    let (x, y) = ui_place::park_point();
    ui_place::park(hwnd, x, y);
}

/// 会话视图转面板输入;页切片、选中态与页码标签与 GPUI 路径同一套语义。
fn spec_of(view: &SessionView, layout: UiLayout) -> PanelSpec {
    let ps = view.page_size.max(1);
    let start = view.page * ps;
    let end = (start + ps).min(view.candidates.len());
    let items = (start..end)
        .map(|i| Item {
            text: view.candidates[i].clone(),
            hint: view.hints.get(i).cloned().unwrap_or_default(),
            selected: i == view.highlight,
        })
        .collect();
    let pages = view.candidates.len().div_ceil(ps).max(1);
    let page_label = if pages > 1 {
        format!("{}/{}", view.page + 1, pages)
    } else {
        String::new()
    };
    PanelSpec {
        width: 0.0,
        // 摆位时按字号档 × DPI 补上;这里先置零。
        max_text_width: 0.0,
        preedit: PreeditBar {
            text: view.raw.clone(),
            page_label,
        },
        items,
        layout: if matches!(layout, UiLayout::Horizontal) {
            Layout::Wrap
        } else {
            Layout::Column
        },
    }
}

/// 全组合候选都参与估宽;翻页时窗口宽度与 GPUI 路径一致,不会来回跳。
fn measure_width(
    view: &SessionView,
    layout: UiLayout,
    metrics: &Metrics,
    font: Option<&Font>,
) -> f32 {
    let mut spec = spec_of(view, layout);
    spec.items = view
        .candidates
        .iter()
        .enumerate()
        .map(|(index, text)| Item {
            text: text.clone(),
            hint: view.hints.get(index).cloned().unwrap_or_default(),
            selected: false,
        })
        .collect();
    panel::panel_width(&spec, metrics, font)
}

/// `theme::Colors` → 渲染令牌;候选窗底色是 surface,bg 是设置窗的。
fn palette_of(c: theme::Colors) -> Palette {
    Palette {
        bg: c.surface,
        surface_alt: c.surface_alt,
        border: c.border,
        text: c.text,
        text_secondary: c.text_secondary,
        text_tertiary: c.text_tertiary,
        accent: c.accent,
        accent_text: c.accent_text,
        selected_bg: c.selected_bg,
        selected_text: c.selected_text,
        hover: c.hover,
    }
}

/// CI 自检:建窗、Ganesh-D3D12 出帧、读回像素断言,结论写日志。
/// 与 Linux 光栅 E2E 同一套期望值(无字体估算路径),失败由 win-e2e 断言。
/// 没有桌面会话或合成交换链不可用时退回离屏渲染,像素期望不变。
pub(super) fn selftest() {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // 固定 100% 缩放与无字体估算路径,像素期望与 Linux E2E 完全一致。
    let metrics = Metrics::standard();
    let spec = demo_spec();
    let width = panel::panel_width(&spec, &metrics, None);
    let height = panel::panel_height(&spec, &metrics, None);
    let palette = palette_of(theme::colors_with(theme::Appearance::Light, 0));
    let (mode, outcome) = match window_target(width as u32, height as u32) {
        Ok((mut pipe, hwnd)) => {
            let outcome = check_pixels(&mut pipe, &spec, &palette, &metrics);
            // 先放管线再销窗:DComp 目标还指着这个 HWND。
            drop(pipe);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            ("合成交换链", outcome)
        }
        Err(e) => {
            // 桌面会话缺失或合成交换链建不起来时,退回离屏渲染验证出帧。
            println!("SKIA 自检降级:合成交换链不可用({e}),改走离屏渲染");
            match Offscreen::new(width as u32, height as u32) {
                Ok(mut target) => (
                    "离屏渲染",
                    check_pixels(&mut target, &spec, &palette, &metrics),
                ),
                Err(e) => {
                    println!("SKIA 自检失败:离屏渲染建管线 {e}");
                    return;
                }
            }
        }
    };
    match outcome {
        Ok(()) => println!(
            "SKIA 自检通过:{mode}帧读回一致({}x{})",
            width as u32, height as u32
        ),
        Err(e) => println!("SKIA 自检失败:{mode} {e}"),
    }
}

/// 建窗 + DComp 交换链;失败时把原因带回去,自检据此决定是否降级。
fn window_target(w: u32, h: u32) -> Result<(Pipeline, HWND), String> {
    let hwnd = create_window().map_err(|e| format!("建窗 {e}"))?;
    let mut pipe = match Pipeline::new(hwnd) {
        Ok(pipe) => pipe,
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(e);
        }
    };
    if let Err(e) = pipe.rebuild_surfaces(w, h) {
        drop(pipe);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return Err(e);
    }
    Ok((pipe, hwnd))
}

/// 两帧的像素断言:静态面板的底色与拼音条,再验悬停与菜单的叠加顺序。
/// 窗口路径与离屏路径共用同一套期望值。
fn check_pixels(
    target: &mut dyn FrameTarget,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
) -> Result<(), String> {
    // 第一帧:静态面板,读回底色与拼音条。
    target.draw(Frame {
        spec,
        palette,
        metrics,
        font: None,
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: None,
        pager: None,
        menu: None,
        menu_hover: None,
    })?;
    let expected_bg = rgb(palette.bg);
    let expected_bar = rgb(palette.surface_alt);
    let bg = target.pixel(5, 60).ok_or("像素读回失败(背景)")?;
    if bg != expected_bg {
        return Err(format!("背景像素 {bg:?} 期望 {expected_bg:?}"));
    }
    let bar = target.pixel(5, 10).ok_or("像素读回失败(拼音条)")?;
    if bar != expected_bar {
        return Err(format!("拼音条像素 {bar:?} 期望 {expected_bar:?}"));
    }
    // 第二帧:第二行悬停 + 菜单压住它左半,验证叠加顺序与菜单底色。
    let rects = panel::item_rects(spec, metrics, None);
    let hover_at = ((rects[1].right - 6.0) as i32, (rects[1].top + 6.0) as i32);
    let covered_at = ((rects[1].left + 6.0) as i32, (rects[1].top + 6.0) as i32);
    target.draw(Frame {
        spec,
        palette,
        metrics,
        font: None,
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: Some(1),
        pager: None,
        menu: Some((8.0, 40.0)),
        menu_hover: None,
    })?;
    let hover = target
        .pixel(hover_at.0, hover_at.1)
        .ok_or("像素读回失败(悬停)")?;
    let expected_hover = rgb(palette.hover);
    if hover != expected_hover {
        return Err(format!("悬停像素 {hover:?} 期望 {expected_hover:?}"));
    }
    let covered = target
        .pixel(covered_at.0, covered_at.1)
        .ok_or("像素读回失败(菜单)")?;
    if covered != expected_bg {
        return Err(format!("菜单像素 {covered:?} 期望 {expected_bg:?}"));
    }
    Ok(())
}

/// 自检用的固定面板;内容与 Linux E2E 的 `render::spec()` 一致。
fn demo_spec() -> PanelSpec {
    PanelSpec {
        width: 360.0,
        max_text_width: 490.0,
        preedit: PreeditBar {
            text: "nihao".into(),
            page_label: "1/2".into(),
        },
        items: vec![
            Item {
                text: "你好".into(),
                hint: String::new(),
                selected: true,
            },
            Item {
                text: "你号".into(),
                hint: String::new(),
                selected: false,
            },
        ],
        layout: Layout::Wrap,
    }
}
