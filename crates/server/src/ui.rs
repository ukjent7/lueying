//! 候选窗:GPUI 无焦点置顶浮层,轮询会话版本号刷新,贴宿主光标矩形摆放。
//! gpui 未暴露窗口移动/置顶,经 raw-window-handle 拿 HWND 走 SetWindowPos;
//! 空闲时把窗口挪出屏幕而不是隐藏,避免和 gpui 自己的可见性状态打架。

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, App, Bounds, Context, MouseButton, Point, ScrollDelta, SharedString,
    Window, WindowBounds, WindowKind, WindowOptions, div, ease_out_quint, prelude::*, px, rgb,
    size,
};
use gpui_platform::application;
use lueying_protocol::{Request, SessionView};
use lueying_server::theme;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HWND_TOPMOST, IsWindowVisible, IsZoomed, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};
use crate::imp::State;

#[path = "ui_hud.rs"]
mod hud;

use hud::ModeHudPanel;

/// 逻辑尺寸(乘 scale_factor 得物理像素)。
const WIDTH: f32 = 360.0;
/// 整句候选比单词长得多;固定 360 会把十五字以上的句子裁掉。窗口随候选
/// 变宽,到上限后改用省略号,避免盖满半个屏幕。
const MAX_WIDTH: f32 = 680.0;
/// 候选文字在最大窗口宽度下可用的宽度,给序号、注音和内边距留位。
const MAX_TEXT_WIDTH: f32 = MAX_WIDTH - 190.0;
const PREEDIT_H: f32 = 32.0;
const ROW_H: f32 = 34.0;
/// 候选窗与光标之间留一点呼吸,不贴着字形下缘。
const CARET_GAP: f32 = 4.0;
/// 面板底部的内边距。
const PANEL_PAD_B: f32 = 6.0;
/// 翻页时候选内容横向滑入,90ms 内归位。
const PAGE_SLIDE: Duration = Duration::from_millis(90);
/// 空闲停靠位到虚拟桌面左缘的距离。按虚拟桌面算,左侧副屏也不会露出空窗。
const PARK_GAP: i32 = 2000;
/// 首次出现淡入:首帧就有 45% 不透明度,140ms 内到 100%,避免快速打字时候选短暂不可见。
const SHOW_FADE: Duration = Duration::from_millis(140);
const SHOW_START_OPACITY: f32 = 0.45;
/// 组合结束后的淡出时长;期间沿用最后一份内容,结束后再停靠窗口。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 中英切换徽标:紧凑圆角卡片,120ms 淡入,600ms 后开始 180ms 淡出。
const HUD_W: f32 = 46.0;
const HUD_H: f32 = 32.0;
const HUD_FADE_IN: Duration = Duration::from_millis(120);
const HUD_START_OPACITY: f32 = 0.7;
const HUD_SHOW: Duration = Duration::from_millis(600);
const HUD_FADE: Duration = Duration::from_millis(180);
const HUD_TOTAL: Duration = Duration::from_millis(780);

pub(super) fn run(state: Arc<State>) {
    application().run(move |cx: &mut App| {
        let (park_x, park_y) = park_point();
        let bounds = Bounds {
            origin: Point::new(px(park_x as f32), px(park_y as f32)),
            size: size(px(WIDTH), px(PREEDIT_H + ROW_H + PANEL_PAD_B)),
        };
        let panel_state = Arc::clone(&state);
        let handle = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: false,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    kind: WindowKind::PopUp,
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| CandidatePanel {
                        state: panel_state,
                        styled: Cell::new(false),
                        last: RefCell::new(None),
                        fading: Cell::new(None),
                        above: Cell::new(None),
                        page: Cell::new(None),
                        slide: Cell::new(None),
                        slide_serial: Cell::new(0),
                        reduce: Cell::new(false),
                    })
                },
            )
            .expect("候选窗打开失败");
        // 建窗后先摆一次:补上无焦点/工具窗样式并挪出屏幕。只等首个按键
        // 才摆位的话,候选窗在 Alt+Tab/任务栏里会以普通窗口出现一次。
        let _ = handle.update(cx, |panel: &mut CandidatePanel, window, _| {
            panel.place(window);
        });
        // 中英切换徽标:独立小窗,同样无焦点、置顶、鼠标穿透。
        let hud_state = Arc::clone(&state);
        let hud = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Point::new(px(park_x as f32), px(park_y as f32)),
                        size: size(px(HUD_W), px(HUD_H)),
                    })),
                    focus: false,
                    is_movable: false,
                    is_resizable: false,
                    is_minimizable: false,
                    kind: WindowKind::PopUp,
                    ..Default::default()
                },
                |_, cx| cx.new(|_| ModeHudPanel::new(hud_state)),
            )
            .expect("模式徽标窗打开失败");
        let _ = hud.update(cx, |hud: &mut ModeHudPanel, window, _| {
            hud.place(window);
        });
        // 轮询版本号,变了才摆位刷新;15ms 对打字反馈无感。
        let poll = Arc::clone(&state);
        cx.spawn(async move |cx| {
            let mut last = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(15))
                    .await;
                let version = poll.version.load(Ordering::Relaxed);
                let changed = version != last;
                last = version;
                let _ = handle.update(cx, |panel: &mut CandidatePanel, window, cx| {
                    // 淡出结束也要进来一次:清掉旧内容并把窗口停靠出去。
                    if changed || panel.tick() {
                        panel.place(window);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        // 徽标只在切换后短暂显示;淡出期间 15ms 重绘一次透明度。
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(15))
                    .await;
                let _ = hud.update(cx, |hud: &mut ModeHudPanel, window, cx| {
                    let (place, redraw) = hud.tick();
                    if place {
                        hud.place(window);
                    }
                    if redraw {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    });
}

/// 激活会话且在组合中才有窗口内容。宿主接管候选绘制时让位。
fn snapshot(state: &State) -> Option<SessionView> {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let session = sessions.get(&state.active.load(Ordering::Relaxed))?;
    (!session.host_draws && session.engine.composition().is_some()).then(|| session.view.clone())
}

/// 组合仍在进行;淡出窗口里的旧候选不能响应鼠标。宿主接管候选绘制时同样算组合中。
fn in_composition(state: &State) -> bool {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    sessions
        .get(&state.active.load(Ordering::Relaxed))
        .is_some_and(|s| s.engine.composition().is_some())
}

/// 服务端候选窗在显示;宿主接管绘制时让位。
fn composing(state: &State) -> bool {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    sessions
        .get(&state.active.load(Ordering::Relaxed))
        .is_some_and(|s| !s.host_draws && s.engine.composition().is_some())
}

/// 从 gpui 窗口取原生 HWND;窗口系统不是 Win32 时返回 None。
fn window_hwnd(window: &Window) -> Option<HWND> {
    let raw = <Window as HasWindowHandle>::window_handle(window)
        .ok()?
        .as_raw();
    let RawWindowHandle::Win32(handle) = raw else {
        return None;
    };
    Some(HWND(handle.hwnd.get() as *mut c_void))
}

/// 无焦点置顶工具窗样式;`click_through` 时再让鼠标穿透到宿主。
fn style_popup(hwnd: HWND, styled: &Cell<bool>, click_through: bool) {
    let old = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let mut style = old;
    if !styled.get() {
        style |= WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0;
        styled.set(true);
    }
    if click_through {
        style |= WS_EX_TRANSPARENT.0;
    } else {
        style &= !WS_EX_TRANSPARENT.0;
    }
    if style != old {
        unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize) };
    }
}

struct CandidatePanel {
    state: Arc<State>,
    /// HWND 扩展样式只补一次。
    styled: Cell<bool>,
    /// 最后一份组合内容;淡出期间继续画它。
    last: RefCell<Option<SessionView>>,
    /// 淡出截止时刻;`None` 表示没有在淡出。
    fading: Cell<Option<Instant>>,
    /// 本次组合里候选窗在光标上方还是下方;同一次组合内保持不变。
    above: Cell<Option<bool>>,
    /// 上一帧的页码,用来判断翻页方向。
    page: Cell<Option<usize>>,
    /// 正在进行的翻页滑动:方向与结束时刻。
    slide: Cell<Option<(f32, Instant)>>,
    /// 翻页动画 id;单调递增,避免复用已结束的动画状态。
    slide_serial: Cell<u64>,
    /// 上一帧读到的系统「减少动态效果」设置。
    reduce: Cell<bool>,
}

impl CandidatePanel {
    /// 淡出到期后丢掉旧内容并停靠窗口。返回是否改了显示状态。
    fn tick(&self) -> bool {
        if self.fading.get().is_some_and(|end| Instant::now() >= end) {
            self.fading.set(None);
            self.last.borrow_mut().take();
            self.above.set(None);
            self.page.set(None);
            self.slide.set(None);
            return true;
        }
        false
    }

    /// 按光标矩形摆窗;无内容则挪出屏幕。必须在主线程(gpui update 闭包内)调用。
    fn place(&self, window: &mut Window) {
        let Some(hwnd) = window_hwnd(window) else {
            return;
        };
        unsafe {
            let view = match snapshot(&self.state) {
                Some(view) => {
                    // 新一次组合重新判断候选窗朝向;同一次组合内沿用,
                    // 候选行数变化时不会在光标上下反复翻跳。
                    if self.last.borrow().is_none() || self.fading.get().is_some() {
                        self.above.set(None);
                        self.page.set(None);
                        self.slide.set(None);
                    }
                    *self.last.borrow_mut() = Some(view.clone());
                    self.fading.set(None);
                    Some(view)
                }
                None => {
                    if self.reduce.get() {
                        // 减少动态效果:不淡出,直接停靠,避免空窗口挡住点击。
                        self.last.borrow_mut().take();
                        self.fading.set(None);
                    } else if self.last.borrow().is_some() && self.fading.get().is_none() {
                        // 组合刚结束:窗口先留在原地淡出,到期由 `tick` 停靠。
                        self.fading.set(Some(Instant::now() + HIDE_FADE));
                    }
                    self.last.borrow().clone()
                }
            };
            // 淡出期间让点击穿透到下层应用;实时组合才接鼠标。
            style_popup(hwnd, &self.styled, self.fading.get().is_some());
            let (park_x, park_y) = park_point();
            let width = view.as_ref().map_or(WIDTH, panel_width);
            // 会话号高 32 位是宿主进程号。浏览器 F11、全屏终端这类宿主
            // 自己铺满屏幕时仍要显示候选窗,只有别的进程独占全屏才让位。
            let host_pid = (self.state.active.load(Ordering::Relaxed) >> 32) as u32;
            let (x, y, w, h) = match view.as_ref().and_then(|v| v.caret) {
                Some((l, t, r, b)) => {
                    let left = l.min(r);
                    // 候选窗通常停在外屏,`window.scale_factor()` 是停靠屏的 DPI;
                    // 必须按光标所在显示器算,否则副屏 150%/200% 下窗口尺寸不匹配。
                    let scale = scale_at(left, b);
                    let rows = view.as_ref().map_or(1, |v| panel_rows(v, width));
                    let h = ((PREEDIT_H + ROW_H * rows as f32 + PANEL_PAD_B) * scale) as i32;
                    let mut w = (width * scale) as i32;
                    // 按光标所在屏的工作区夹取。
                    let work = work_area_at(left, b);
                    w = w.min((work.right - work.left).max(1));
                    // 同一次组合内优先留在已选的一侧;只有这一侧放不下才翻边。
                    // 副屏可以位于主屏上方,`top` 是负数,不能直接 `.max(0)`。
                    let gap = (CARET_GAP * scale) as i32;
                    let below = b + gap;
                    let above = t - h - gap;
                    let fits_below = below + h <= work.bottom;
                    let fits_above = above >= work.top;
                    let prefer_above = match self.above.get() {
                        Some(previous) if previous && fits_above => true,
                        Some(previous) if !previous && fits_below => false,
                        _ => !fits_below && fits_above,
                    };
                    self.above.set(Some(prefer_above));
                    let y = (if prefer_above { above } else { below })
                        .clamp(work.top, (work.bottom - h).max(work.top));
                    let x = left.clamp(work.left, (work.right - w).max(work.left));
                    // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
                    // 最大化窗口保留候选窗(它的矩形通常只到工作区)。
                    let hidden = foreground_fullscreen_at(left, b, host_pid);
                    (x, y, if hidden { 0 } else { w }, h)
                }
                None => {
                    self.above.set(None);
                    (park_x, park_y, 0, 0)
                }
            };
            if w == 0 {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    park_x,
                    park_y,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOSIZE,
                );
            } else {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
            }
        }
    }
}

/// 把窗口挪出虚拟桌面并收起,避免在任务栏/Alt+Tab 里露出空窗。
fn park(hwnd: HWND, x: i32, y: i32) {
    unsafe {
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE,
        );
    }
}

/// 单条候选的估算宽度(内边距、序号、文字与注音)。
/// 17px 微软雅黑里 CJK/全角约 17px,ASCII 约 9px;按字符类别累加,
/// 避免整句候选估窄后多折一行、窗口高度不够被裁。
fn item_width(text: &str, hint: Option<&String>) -> f32 {
    let hint_width = hint
        .filter(|hint| !hint.is_empty())
        .map_or(0.0, |hint| 4.0 + 11.0 * text_units(hint))
        .min(100.0);
    let hint_gap = if hint_width > 0.0 { 4.0 } else { 0.0 };
    // px_2(16)+强调条(2)+间距(4)+序号(12)+间距(4)+候选文字+注音。
    16.0 + 2.0 + 4.0 + 12.0 + 4.0 + 17.0 * text_units(text) + hint_gap + hint_width
}

/// 文字宽度折算成字号倍数:ASCII 约半个字宽,其余按全角算。
fn text_units(text: &str) -> f32 {
    text.chars()
        .map(|c| if c.is_ascii() { 0.53 } else { 1.0 })
        .sum()
}

/// 候选窗的估算高度(逻辑像素):预编辑行 + 当前页候选行 + 底部内边距。
fn candidate_height(view: &SessionView) -> f32 {
    PREEDIT_H + ROW_H * panel_rows(view, panel_width(view)) as f32 + PANEL_PAD_B
}

/// 整个组合的窗口宽度:按全部候选估最宽一条,翻页时不来回跳。
fn panel_width(view: &SessionView) -> f32 {
    let widest = view
        .candidates
        .iter()
        .enumerate()
        .map(|(index, text)| item_width(text, view.hints.get(index)))
        .fold(0.0, f32::max);
    (widest + 18.0).clamp(WIDTH, MAX_WIDTH)
}

/// 当前页候选估算占几行(按字数估宽,避免窗口比内容高出几行)。
fn panel_rows(view: &SessionView, width: f32) -> usize {
    let ps = view.page_size.max(1);
    let start = view.page * ps;
    let end = (start + ps).min(view.candidates.len());
    let mut rows = 0usize;
    let mut used = 0.0f32;
    // 面板左右内边距 16 + 边框 2;可用宽度少于估算会让窗口少算一行。
    let available = width - 18.0;
    for index in start..end {
        let item = item_width(&view.candidates[index], view.hints.get(index));
        if rows == 0 {
            rows = 1;
            used = item;
        } else if used + item + 4.0 > available {
            rows += 1;
            used = item;
        } else {
            used += item + 4.0;
        }
    }
    rows
}

/// 候选窗里的翻页箭头;滚轮之外再给鼠标一个明确入口。
fn page_arrow(
    label: &'static str,
    delta: i32,
    state: Arc<State>,
    colors: theme::Colors,
) -> impl IntoElement {
    div()
        // `active` 只挂在有状态元素上;两个翻页键用不同 id。
        .id(if delta < 0 { "page-prev" } else { "page-next" })
        .flex()
        .items_center()
        .justify_center()
        .w(px(22.0))
        .h(px(22.0))
        .rounded_md()
        .text_size(px(15.0))
        .text_color(rgb(colors.text_secondary))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(colors.border)))
        .active(|style| style.opacity(0.75))
        .on_mouse_down(MouseButton::Left, move |_, _, _| {
            let session = state.active.load(Ordering::Relaxed);
            if session != 0 && composing(&state) {
                let _ = crate::imp::handle(&state, Request::Page { session, delta });
            }
        })
        .child(label)
}

/// 虚拟桌面左缘之外的停靠点。固定负坐标会落在左侧副屏上,露出空白候选窗。
fn park_point() -> (i32, i32) {
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    (left.saturating_sub(PARK_GAP), top)
}

fn monitor_at(x: i32, y: i32) -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) }
}

fn monitor_info_at(x: i32, y: i32) -> Option<MONITORINFO> {
    monitor_info(monitor_at(x, y))
}

fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFO> {
    if monitor.is_invalid() {
        return None;
    }
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then_some(info)
}

/// 主屏工作区;没有光标位置时徽标退到这里的下三分之一处。
fn primary_work_area() -> RECT {
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    monitor_info(monitor).map_or(
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        },
        |info| info.rcWork,
    )
}

/// 光标所在显示器的缩放比例;取不到时按 100%。
fn scale_at(x: i32, y: i32) -> f32 {
    let monitor = monitor_at(x, y);
    if monitor.is_invalid() {
        return 1.0;
    }
    let mut dpi_x = 0u32;
    let mut dpi_y = 0u32;
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok()
        && dpi_x > 0
    {
        dpi_x as f32 / 96.0
    } else {
        1.0
    }
}

/// 光标所在显示器的工作区(物理像素);取不到时给常见主屏兜底。
fn work_area_at(x: i32, y: i32) -> RECT {
    monitor_info_at(x, y).map_or(
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        },
        |info| info.rcWork,
    )
}

/// 前台窗口是否铺满光标所在显示器。最大化窗口不算(任务栏仍在),
/// 这样 F11/独占全屏游戏不弹候选窗,普通最大化应用照常输入。
/// `host_pid` 是当前输入会话的宿主进程:它自己的全屏窗口仍要显示候选窗。
fn foreground_fullscreen_at(x: i32, y: i32, host_pid: u32) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid()
        || !unsafe { IsWindowVisible(foreground) }.as_bool()
        || unsafe { IsZoomed(foreground) }.as_bool()
    {
        return false;
    }
    if host_pid != 0 {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
        if pid == host_pid {
            return false;
        }
    }
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(foreground, &mut rect) }.is_err() {
        return false;
    }
    let Some(info) = monitor_info_at(x, y) else {
        return false;
    };
    let screen = info.rcMonitor;
    rect.left <= screen.left
        && rect.top <= screen.top
        && rect.right >= screen.right
        && rect.bottom >= screen.bottom
}

impl Render for CandidatePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reduce.set(cx.reduce_motion());
        let live = snapshot(&self.state);
        // 组合结束后的淡出沿用最后一份内容;旧内容已清掉则什么都不画。
        let fading = live.is_none();
        let Some(view) = live.or_else(|| self.last.borrow().clone()) else {
            return div().into_any_element();
        };
        let appearance = theme::Appearance::of(window);
        let colors = theme::colors(appearance);
        let width = panel_width(&view);
        let ps = view.page_size.max(1);
        let start = view.page * ps;
        let pages = view.candidates.len().div_ceil(ps).max(1);
        let page_no = view.page + 1;
        // 只有翻页才滑动;首次展开和继续打字不横向跳。
        if self.page.get().is_some_and(|page| page != view.page) {
            let previous = self.page.get().unwrap_or(view.page);
            let direction = if view.page > previous { 1.0 } else { -1.0 };
            self.slide
                .set(Some((direction, Instant::now() + PAGE_SLIDE)));
            self.slide_serial.set(self.slide_serial.get() + 1);
        }
        self.page.set(Some(view.page));
        let sliding = self
            .slide
            .get()
            .filter(|(_, until)| Instant::now() < *until);
        if sliding.is_none() {
            self.slide.set(None);
        }
        let slide_serial = self.slide_serial.get();
        let items: Vec<(usize, String, String, bool)> = view
            .candidates
            .iter()
            .enumerate()
            .skip(start)
            .take(ps)
            .map(|(i, text)| {
                (
                    i - start,
                    text.clone(),
                    view.hints.get(i).cloned().unwrap_or_default(),
                    i == view.highlight,
                )
            })
            .collect();
        let has_items = !items.is_empty();
        let state = Arc::clone(&self.state);
        let scroll_state = Arc::clone(&self.state);
        let previous = Arc::clone(&self.state);
        let next = Arc::clone(&self.state);
        let preedit = SharedString::from(view.raw.clone());
        let panel =
            div()
                .font_family("Microsoft YaHei UI")
                .flex()
                .flex_col()
                .w(px(width))
                .bg(rgb(colors.surface))
                .border_1()
                .border_color(rgb(colors.border))
                .rounded_lg()
                .overflow_hidden()
                .on_scroll(move |event, _, _| {
                    let dir = match event.delta {
                        ScrollDelta::Lines(p) => p.y.signum(),
                        ScrollDelta::Pixels(p) => (p.y / px(1.0)).signum(),
                    };
                    if dir == 0.0 {
                        return;
                    }
                    let session = scroll_state.active.load(Ordering::Relaxed);
                    if session != 0 && composing(&scroll_state) {
                        let _ = crate::imp::handle(
                            &scroll_state,
                            Request::Page {
                                session,
                                delta: if dir > 0.0 { -1 } else { 1 },
                            },
                        );
                    }
                })
                .child(
                    div()
                        .flex()
                        .h(px(PREEDIT_H))
                        .px_3()
                        .gap_2()
                        .items_center()
                        .justify_between()
                        .bg(rgb(colors.surface_alt))
                        // 只有当前页有候选时才分隔拼音与候选,空页不留一条孤线。
                        .when(has_items, |el| {
                            el.border_b_1().border_color(rgb(colors.border))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(px(13.0))
                                .text_color(rgb(colors.text_secondary))
                                .child(preedit),
                        )
                        .child(if pages > 1 {
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(page_arrow("‹", -1, previous, colors))
                                .child(
                                    div()
                                        .min_w(px(28.0))
                                        .text_center()
                                        .text_size(px(11.0))
                                        .text_color(rgb(colors.text_tertiary))
                                        .child(format!("{page_no}/{pages}")),
                                )
                                .child(page_arrow("›", 1, next, colors))
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                )
                .children(has_items.then(|| {
                    let list = div().flex().flex_wrap().px_2().py_1().gap_1().children(
                        items.into_iter().map(|(slot, text, hint, hl)| {
                            let state = Arc::clone(&state);
                            div()
                                // `active` 只挂在有状态元素上;槽位在页内唯一。
                                .id(("cand", slot))
                                .flex()
                                .items_center()
                                .h(px(30.0))
                                .px_2()
                                .rounded_md()
                                .cursor_pointer()
                                .active(|el| el.opacity(0.82))
                                .text_size(px(17.0))
                                .when(hl, |el| {
                                    el.bg(rgb(colors.selected_bg))
                                        .text_color(rgb(colors.selected_text))
                                })
                                .when(!hl, |el| {
                                    el.text_color(rgb(colors.text))
                                        .hover(|s| s.bg(rgb(colors.hover)))
                                })
                                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                                    let session = state.active.load(Ordering::Relaxed);
                                    if session != 0 && composing(&state) {
                                        let _ = crate::imp::handle(
                                            &state,
                                            Request::Pick { session, slot },
                                        );
                                    }
                                })
                                .child(
                                    div()
                                        .w(px(2.0))
                                        .h(px(14.0))
                                        .rounded_full()
                                        .when(hl, |el| el.bg(rgb(colors.accent_text))),
                                )
                                .child(
                                    div()
                                        .w(px(12.0))
                                        .ml(px(4.0))
                                        .text_size(px(11.0))
                                        .text_color(if hl {
                                            rgb(colors.accent_text)
                                        } else {
                                            rgb(colors.text_tertiary)
                                        })
                                        .child(format!("{}", slot + 1)),
                                )
                                .child(
                                    div()
                                        .ml(px(4.0))
                                        .max_w(px(MAX_TEXT_WIDTH))
                                        .truncate()
                                        .child(SharedString::from(text)),
                                )
                                .when(!hint.is_empty(), |el| {
                                    el.child(
                                        div()
                                            .ml(px(4.0))
                                            .max_w(px(100.0))
                                            .truncate()
                                            .text_size(px(11.0))
                                            .text_color(if hl {
                                                rgb(colors.accent_text)
                                            } else {
                                                rgb(colors.text_tertiary)
                                            })
                                            .child(SharedString::from(hint)),
                                    )
                                })
                        }),
                    );
                    if let Some((direction, _)) = sliding {
                        list.with_animation(
                            ("page-slide", slide_serial),
                            Animation::new(PAGE_SLIDE).with_easing(ease_out_quint()),
                            move |list, delta| {
                                list.relative()
                                    .left(px(direction * 10.0 * (1.0 - delta)))
                                    .opacity(0.55 + 0.45 * delta)
                            },
                        )
                        .into_any_element()
                    } else {
                        list.into_any_element()
                    }
                }));
        // 首次出现 140ms 淡入(首帧 45% 不透明度,候选立即可读),组合结束
        // 90ms 淡出;GPUI 的 reduced-motion 会自动给终态,淡出期间点击由
        // `composing` 拦掉。
        if fading {
            panel.with_animation(
                "candidate-hide",
                Animation::new(HIDE_FADE).with_easing(ease_out_quint()),
                |panel, delta| panel.opacity(1.0 - delta),
            )
            .into_any_element()
        } else {
            panel.with_animation(
                "candidate-show",
                Animation::new(SHOW_FADE).with_easing(ease_out_quint()),
                |panel, delta| {
                    panel.opacity(SHOW_START_OPACITY + (1.0 - SHOW_START_OPACITY) * delta)
                },
            )
            .into_any_element()
        }
    }
}
