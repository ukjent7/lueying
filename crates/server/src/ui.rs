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
    Window, WindowAppearance, WindowBounds, WindowKind, WindowOptions, div, ease_out_quint,
    prelude::*, px, rgb, size,
};
use gpui_platform::application;
use lueying_protocol::{Request, SessionView};
use lueying_server::theme;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    COLOR_3DFACE, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWFRAME,
    COLOR_WINDOWTEXT, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, HWND_TOPMOST, IsWindowVisible, IsZoomed, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};

use crate::imp::State;

/// 逻辑尺寸(乘 scale_factor 得物理像素)。
const WIDTH: f32 = 340.0;
/// 整句候选比单词长得多;固定 340 会把十五字以上的句子裁掉。窗口随候选
/// 变宽,到上限后改用省略号,避免盖满半个屏幕。
const MAX_WIDTH: f32 = 680.0;
/// 候选文字在最大窗口宽度下可用的宽度,给序号、注音和内边距留位。
const MAX_TEXT_WIDTH: f32 = MAX_WIDTH - 170.0;
const PREEDIT_H: f32 = 30.0;
const ROW_H: f32 = 36.0;
/// 空闲停靠位到虚拟桌面左缘的距离。按虚拟桌面算,左侧副屏也不会露出空窗。
const PARK_GAP: i32 = 2000;
/// 组合结束后的淡出时长;期间沿用最后一份内容,结束后再停靠窗口。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 中英切换徽标:紧凑圆角卡片,完全显示 600ms,再用 180ms 淡出。
const HUD_W: f32 = 46.0;
const HUD_H: f32 = 32.0;
const HUD_SHOW: Duration = Duration::from_millis(600);
const HUD_FADE: Duration = Duration::from_millis(180);
const HUD_TOTAL: Duration = Duration::from_millis(780);

pub(super) fn run(state: Arc<State>) {
    application().run(move |cx: &mut App| {
        let (park_x, park_y) = park_point();
        let bounds = Bounds {
            origin: Point::new(px(park_x as f32), px(park_y as f32)),
            size: size(px(WIDTH), px(PREEDIT_H + ROW_H)),
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
                |_, cx| {
                    cx.new(|_| ModeHudPanel {
                        state: hud_state,
                        styled: Cell::new(false),
                        serial: Cell::new(0),
                        was_composing: Cell::new(false),
                        last: RefCell::new(None),
                    })
                },
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
    /// 上一帧读到的系统「减少动态效果」设置。
    reduce: Cell<bool>,
}

impl CandidatePanel {
    /// 淡出到期后丢掉旧内容并停靠窗口。返回是否改了显示状态。
    fn tick(&self) -> bool {
        if self.fading.get().is_some_and(|end| Instant::now() >= end) {
            self.fading.set(None);
            self.last.borrow_mut().take();
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
                    let mut w = (width * scale) as i32;
                    let h = ((PREEDIT_H + ROW_H * rows as f32 + 8.0) * scale) as i32;
                    // 按光标所在屏的工作区夹取。
                    let work = work_area_at(left, b);
                    w = w.min((work.right - work.left).max(1));
                    let mut x = left;
                    let mut y = b;
                    if y + h > work.bottom {
                        // 副屏可以位于主屏上方,`top` 是负数;按光标所在屏的
                        // 工作区上沿夹取,不能直接 `.max(0)`。
                        y = (t - h).max(work.top);
                    }
                    x = x.clamp(work.left, (work.right - w).max(work.left));
                    // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
                    // 最大化窗口保留候选窗(它的矩形通常只到工作区)。
                    let hidden = foreground_fullscreen_at(left, b, host_pid);
                    (x, y, if hidden { 0 } else { w }, h)
                }
                None => (park_x, park_y, 0, 0),
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

/// 当前这份徽标:显示哪个字、贴哪个光标、从何时开始计时。
#[derive(Clone)]
struct HudView {
    label: &'static str,
    caret: Option<(i32, i32, i32, i32)>,
    at: Instant,
}

struct ModeHudPanel {
    state: Arc<State>,
    /// HWND 扩展样式只补一次。
    styled: Cell<bool>,
    /// 已处理的切换序号;同一序号不重复读取状态。
    serial: Cell<u64>,
    /// 上一帧是否在组合;组合开始/结束时挪开或贴回光标下方。
    was_composing: Cell<bool>,
    last: RefCell<Option<HudView>>,
}

impl ModeHudPanel {
    /// 轮询切换序号与显示期限。返回(是否重新摆位, 是否重绘)。
    fn tick(&self) -> (bool, bool) {
        let serial = self.state.hud_serial.load(Ordering::Relaxed);
        let composing_now = in_composition(&self.state);
        let moved = self.was_composing.replace(composing_now) != composing_now;
        let mut place = false;
        if serial != self.serial.get() {
            self.serial.set(serial);
            let source = self.state.mode_hud.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(hud) = source.as_ref() {
                *self.last.borrow_mut() = Some(HudView {
                    label: hud.label,
                    caret: hud.caret,
                    at: hud.at,
                });
            }
            place = true;
        }
        let Some(view) = self.last.borrow().clone() else {
            return (place, place);
        };
        if Instant::now().saturating_duration_since(view.at) >= HUD_TOTAL {
            self.last.borrow_mut().take();
            (true, true)
        } else {
            (place || moved, true)
        }
    }

    /// 平时贴在光标下方;下方放不下就翻到上方,组合中按候选窗翻向选相反的一侧,没有光标时落在主屏下三分之一。
    fn place(&self, window: &Window) {
        let Some(hwnd) = window_hwnd(window) else {
            return;
        };
        // 徽标永远鼠标穿透,不挡宿主的选择与点击。
        style_popup(hwnd, &self.styled, true);
        let (park_x, park_y) = park_point();
        let Some(view) = self.last.borrow().clone() else {
            park(hwnd, park_x, park_y);
            return;
        };
        let caret = view
            .caret
            .map(|(l, t, r, b)| (l.min(r), t.min(b), l.max(r), b.max(t)));
        let host_pid = (self.state.active.load(Ordering::Relaxed) >> 32) as u32;
        let (anchor_x, anchor_y, work) = match caret {
            Some((l, _, r, b)) => (l + (r - l) / 2, b, work_area_at(l, b)),
            None => {
                let work = primary_work_area();
                (
                    work.left + (work.right - work.left) / 2,
                    work.top + (work.bottom - work.top) * 2 / 3,
                    work,
                )
            }
        };
        if foreground_fullscreen_at(anchor_x, anchor_y, host_pid) {
            park(hwnd, park_x, park_y);
            return;
        }
        let scale = scale_at(anchor_x, anchor_y);
        let w = (HUD_W * scale) as i32;
        let h = (HUD_H * scale) as i32;
        let gap = (4.0 * scale) as i32;
        // 候选窗默认贴光标下方、放不下才翻到上方;徽标选相反的一侧,两者不会互相遮挡。
        let in_comp = in_composition(&self.state);
        let candidate_h = snapshot(&self.state).map_or(0.0, |view| candidate_height(&view)) * scale;
        let candidate_below = caret
            .is_some_and(|(_, _, _, b)| b as f32 + candidate_h <= work.bottom as f32);
        let prefer_below = !in_comp || !candidate_below;
        let x = (anchor_x - w / 2).clamp(work.left, (work.right - w).max(work.left));
        let y = (match caret {
            Some((_, t, _, b)) => {
                let below = b + gap + h <= work.bottom;
                let above = t - h - gap >= work.top;
                if prefer_below && below {
                    b + gap
                } else if !prefer_below && above {
                    t - h - gap
                } else if below {
                    b + gap
                } else {
                    t - h - gap
                }
            }
            None => anchor_y - h / 2,
        })
        .clamp(work.top, (work.bottom - h).max(work.top));
        unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE) };
    }
}

impl Render for ModeHudPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(view) = self.last.borrow().clone() else {
            return div().into_any_element();
        };
        let colors = if theme::high_contrast() {
            system_palette()
        } else {
            palette(matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ))
        };
        let opacity = if cx.reduce_motion() {
            1.0
        } else {
            let elapsed = Instant::now().saturating_duration_since(view.at);
            if elapsed >= HUD_TOTAL {
                0.0
            } else if elapsed >= HUD_SHOW {
                ((HUD_TOTAL - elapsed).as_secs_f32() / HUD_FADE.as_secs_f32()).clamp(0.0, 1.0)
            } else {
                1.0
            }
        };
        div()
            .font_family("Microsoft YaHei")
            .flex()
            .items_center()
            .justify_center()
            .w(px(HUD_W))
            .h(px(HUD_H))
            .bg(rgb(colors.bg))
            .border_1()
            .border_color(rgb(colors.border))
            .rounded_lg()
            .shadow_lg()
            .text_size(px(18.0))
            .text_color(rgb(colors.text))
            .opacity(opacity)
            .child(view.label)
            .into_any_element()
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
        .min(110.0);
    let hint_gap = if hint_width > 0.0 { 4.0 } else { 0.0 };
    // px_2 + 序号(两位内)+ 序号与候选之间的 gap_1。
    16.0 + 11.0 + 4.0 + 17.0 * text_units(text) + hint_gap + hint_width
}

/// 文字宽度折算成字号倍数:ASCII 约半个字宽,其余按全角算。
fn text_units(text: &str) -> f32 {
    text.chars()
        .map(|c| if c.is_ascii() { 0.53 } else { 1.0 })
        .sum()
}

/// 服务端候选窗的估算高度(逻辑像素):预编辑行 + 当前页候选行 + 底部内边距。
fn candidate_height(view: &SessionView) -> f32 {
    PREEDIT_H + ROW_H * panel_rows(view, panel_width(view)) as f32 + 8.0
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
    colors: Palette,
) -> impl IntoElement {
    div()
        .px_1()
        .rounded_md()
        .text_size(px(13.0))
        .text_color(rgb(colors.index))
        .hover(|style| style.bg(rgb(colors.hover)))
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

#[derive(Clone, Copy)]
struct Palette {
    bg: u32,
    border: u32,
    preedit: u32,
    text: u32,
    index: u32,
    hover: u32,
    selected_bg: u32,
    selected_text: u32,
    selected_index: u32,
}

/// 系统高对比度主题:用用户选定的系统颜色,不覆盖成自定义蓝底。
fn system_palette() -> Palette {
    let text = theme::system_color(COLOR_WINDOWTEXT);
    Palette {
        bg: theme::system_color(COLOR_WINDOW),
        border: theme::system_color(COLOR_WINDOWFRAME),
        preedit: text,
        text,
        // 候选编号是辅助信息,但高对比度下也要清楚可读。
        index: text,
        hover: theme::system_color(COLOR_3DFACE),
        selected_bg: theme::system_color(COLOR_HIGHLIGHT),
        selected_text: theme::system_color(COLOR_HIGHLIGHTTEXT),
        selected_index: theme::system_color(COLOR_HIGHLIGHTTEXT),
    }
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: 0x2b2b31,
            border: 0x44444d,
            preedit: 0xb0b0ba,
            text: 0xe8e8ee,
            index: 0x8a8a96,
            hover: 0x3a3a42,
            selected_bg: 0x3b78ff,
            selected_text: 0xffffff,
            selected_index: 0xd0e0ff,
        }
    } else {
        Palette {
            bg: 0xf7f7f9,
            border: 0xd9d9e0,
            preedit: 0x50505a,
            text: 0x20202a,
            index: 0x90909a,
            hover: 0xe9e9f0,
            selected_bg: 0x2f6fed,
            selected_text: 0xffffff,
            selected_index: 0xc8d8ff,
        }
    }
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
        let colors = if theme::high_contrast() {
            system_palette()
        } else {
            palette(matches!(
                window.appearance(),
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ))
        };
        let width = panel_width(&view);
        let ps = view.page_size.max(1);
        let start = view.page * ps;
        let pages = view.candidates.len().div_ceil(ps).max(1);
        let page_no = view.page + 1;
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
        let state = Arc::clone(&self.state);
        let scroll_state = Arc::clone(&self.state);
        let previous = Arc::clone(&self.state);
        let next = Arc::clone(&self.state);
        let preedit = SharedString::from(view.raw.clone());
        let panel = div()
            .font_family("Microsoft YaHei")
            .flex()
            .flex_col()
            .w(px(width))
            .bg(rgb(colors.bg))
            .border_1()
            .border_color(rgb(colors.border))
            .rounded_lg()
            .shadow_lg()
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
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .max_w(px(220.0))
                            .truncate()
                            .text_size(px(14.0))
                            .text_color(rgb(colors.preedit))
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
                                    .text_size(px(11.0))
                                    .text_color(rgb(colors.index))
                                    .child(format!("{page_no}/{pages}")),
                            )
                            .child(page_arrow("›", 1, next, colors))
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .children((!items.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_wrap()
                    .px_2()
                    .pb_2()
                    .gap_1()
                    .children(items.into_iter().map(|(slot, text, hint, hl)| {
                        let state = Arc::clone(&state);
                        div()
                            .flex()
                            .items_baseline()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
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
                                    let _ =
                                        crate::imp::handle(&state, Request::Pick { session, slot });
                                }
                            })
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(if hl {
                                        rgb(colors.selected_index)
                                    } else {
                                        rgb(colors.index)
                                    })
                                    .child(format!("{}.", slot + 1)),
                            )
                            .child(
                                div()
                                    .max_w(px(MAX_TEXT_WIDTH))
                                    .truncate()
                                    .child(SharedString::from(text)),
                            )
                            .when(!hint.is_empty(), |el| {
                                el.child(
                                    div()
                                        .max_w(px(110.0))
                                        .truncate()
                                        .text_size(px(11.0))
                                        .text_color(if hl {
                                            rgb(colors.selected_index)
                                        } else {
                                            rgb(colors.index)
                                        })
                                        .child(SharedString::from(hint)),
                                )
                            })
                    }))
            }));
        // 首次出现 140ms 淡入,组合结束 90ms 淡出;GPUI 的 reduced-motion
        // 会自动给终态,淡出期间点击由 `composing` 拦掉。
        if fading {
            panel
                .with_animation(
                    "candidate-hide",
                    Animation::new(HIDE_FADE).with_easing(ease_out_quint()),
                    |panel, delta| panel.opacity(1.0 - delta),
                )
                .into_any_element()
        } else {
            panel
                .with_animation(
                    "candidate-show",
                    Animation::new(Duration::from_millis(140)).with_easing(ease_out_quint()),
                    |panel, delta| panel.opacity(delta),
                )
                .into_any_element()
        }
    }
}
