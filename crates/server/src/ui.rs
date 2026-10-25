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
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST,
    IsWindowVisible, IsZoomed, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};

use crate::imp::State;

/// 逻辑尺寸(乘 scale_factor 得物理像素)。
const WIDTH: f32 = 340.0;
const PREEDIT_H: f32 = 30.0;
const ROW_H: f32 = 36.0;
/// 空闲停靠位:主屏左侧外。
const PARK: i32 = -4000;
/// 组合结束后的淡出时长;期间沿用最后一份内容,结束后再停靠窗口。
const HIDE_FADE: Duration = Duration::from_millis(90);

pub(super) fn run(state: Arc<State>) {
    application().run(move |cx: &mut App| {
        let bounds = Bounds {
            origin: Point::new(px(PARK as f32), px(PARK as f32)),
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
    });
}

/// 激活会话且在组合中才有窗口内容。宿主接管候选绘制时让位。
fn snapshot(state: &State) -> Option<SessionView> {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let session = sessions.get(&state.active.load(Ordering::Relaxed))?;
    (!session.host_draws && session.engine.composition().is_some())
        .then(|| session.view.clone())
}

/// 组合仍在进行;淡出窗口里的旧候选不能响应鼠标。
fn composing(state: &State) -> bool {
    snapshot(state).is_some()
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

    fn hwnd(window: &Window) -> Option<HWND> {
        let raw = <Window as HasWindowHandle>::window_handle(window)
            .ok()?
            .as_raw();
        let RawWindowHandle::Win32(handle) = raw else {
            return None;
        };
        Some(HWND(handle.hwnd.get() as *mut c_void))
    }

    /// 按光标矩形摆窗;无内容则挪出屏幕。必须在主线程(gpui update 闭包内)调用。
    fn place(&self, window: &mut Window) {
        let Some(hwnd) = Self::hwnd(window) else {
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
            let old = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            let mut style = old;
            if !self.styled.get() {
                style |= WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0;
                self.styled.set(true);
            }
            if self.fading.get().is_some() {
                style |= WS_EX_TRANSPARENT.0;
            } else {
                style &= !WS_EX_TRANSPARENT.0;
            }
            if style != old {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
            }
            let (x, y, w, h) = match view.as_ref().and_then(|v| v.caret) {
                Some((l, t, r, b)) => {
                    let left = l.min(r);
                    // 候选窗通常停在外屏,`window.scale_factor()` 是停靠屏的 DPI;
                    // 必须按光标所在显示器算,否则副屏 150%/200% 下窗口尺寸不匹配。
                    let scale = scale_at(left, b);
                    let w = (WIDTH * scale) as i32;
                    let rows = view.as_ref().map_or(1, panel_rows);
                    let h = ((PREEDIT_H + ROW_H * rows as f32 + 8.0) * scale) as i32;
                    // 按光标所在屏的工作区夹取。
                    let work = work_area_at(left, b);
                    let mut x = left;
                    let mut y = b;
                    if y + h > work.bottom {
                        y = (t.max(0) - h).max(work.top);
                    }
                    x = x.clamp(work.left, (work.right - w).max(work.left));
                    // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
                    // 最大化窗口保留候选窗(它的矩形通常只到工作区)。
                    let hidden = foreground_fullscreen_at(left, b);
                    (x, y, if hidden { 0 } else { w }, h)
                }
                None => (PARK, PARK, 0, 0),
            };
            if w == 0 {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    PARK,
                    PARK,
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

/// 当前页候选估算占几行(按字数估宽,避免窗口比内容高出几行)。
fn panel_rows(view: &SessionView) -> usize {
    let ps = view.page_size.max(1);
    let start = view.page * ps;
    let end = (start + ps).min(view.candidates.len());
    let mut rows = 0usize;
    let mut used = 0.0f32;
    for index in start..end {
        let text = &view.candidates[index];
        let hint_width = view
            .hints
            .get(index)
            .filter(|hint| !hint.is_empty())
            .map_or(0.0, |hint| 8.0 + 7.0 * hint.chars().count() as f32)
            .min(110.0);
        let width = 22.0 + 18.0 * text.chars().count() as f32 + hint_width;
        if rows == 0 {
            rows = 1;
            used = width;
        } else if used + width + 4.0 > WIDTH - 16.0 {
            rows += 1;
            used = width;
        } else {
            used += width + 4.0;
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

fn monitor_at(x: i32, y: i32) -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) }
}

fn monitor_info_at(x: i32, y: i32) -> Option<MONITORINFO> {
    let monitor = monitor_at(x, y);
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
fn foreground_fullscreen_at(x: i32, y: i32) -> bool {
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid()
        || !unsafe { IsWindowVisible(foreground) }.as_bool()
        || unsafe { IsZoomed(foreground) }.as_bool()
    {
        return false;
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
        let colors = palette(matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ));
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
            .w(px(WIDTH))
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
                            .child(SharedString::from(text))
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
