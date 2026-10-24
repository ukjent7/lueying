//! 候选窗:GPUI 无焦点置顶浮层,轮询会话版本号刷新,贴宿主光标矩形摆放。
//! gpui 未暴露窗口移动/置顶,经 raw-window-handle 拿 HWND 走 SetWindowPos;
//! 空闲时把窗口挪出屏幕而不是隐藏,避免和 gpui 自己的可见性状态打架。

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui::{
    App, Bounds, Context, Point, SharedString, Window, WindowBounds, WindowKind, WindowOptions,
    div, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use pinyin_protocol::SessionView;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SetWindowLongPtrW,
    SetWindowPos, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

use crate::imp::State;

/// 逻辑尺寸(乘 scale_factor 得物理像素)。
const WIDTH: f32 = 340.0;
const PREEDIT_H: f32 = 30.0;
const ROW_H: f32 = 36.0;
/// 空闲停靠位:主屏左侧外。
const PARK: i32 = -4000;

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
                    })
                },
            )
            .expect("候选窗打开失败");
        // 轮询版本号,变了才摆位刷新;15ms 对打字反馈无感。
        let poll = Arc::clone(&state);
        cx.spawn(async move |cx| {
            let mut last = 0u64;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(15))
                    .await;
                let version = poll.version.load(Ordering::Relaxed);
                if version == last {
                    continue;
                }
                last = version;
                let _ = handle.update(cx, |panel: &mut CandidatePanel, window, cx| {
                    panel.place(window);
                    cx.notify();
                });
            }
        })
        .detach();
    });
}

/// 激活会话且在组合中才有窗口内容。
fn snapshot(state: &State) -> Option<SessionView> {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let session = sessions.get(&state.active.load(Ordering::Relaxed))?;
    session
        .engine
        .composition()
        .is_some()
        .then(|| session.view.clone())
}

struct CandidatePanel {
    state: Arc<State>,
    /// HWND 扩展样式只补一次。
    styled: Cell<bool>,
}

impl CandidatePanel {
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
            if !self.styled.get() {
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                SetWindowLongPtrW(
                    hwnd,
                    GWL_EXSTYLE,
                    (style | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as isize,
                );
                self.styled.set(true);
            }
            let view = snapshot(&self.state);
            let scale = window.scale_factor();
            let (x, y, w, h) = match view.as_ref().and_then(|v| v.caret) {
                Some((l, t, r, b)) => {
                    let w = (WIDTH * scale) as i32;
                    let rows = view.as_ref().map_or(1, page_rows);
                    let h = ((PREEDIT_H + ROW_H * rows as f32 + 8.0) * scale) as i32;
                    let work = work_area(hwnd);
                    let mut x = l.min(r);
                    let mut y = b;
                    if y + h > work.bottom {
                        y = (t.max(0) - h).max(work.top);
                    }
                    x = x.clamp(work.left, (work.right - w).max(work.left));
                    (x, y, w, h)
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

/// 当前页候选行数(候选窗高度估算)。
fn page_rows(view: &SessionView) -> usize {
    let ps = view.page_size.max(1);
    view.candidates.len().saturating_sub(view.page * ps).min(ps)
}

fn work_area(hwnd: HWND) -> RECT {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }
}

impl Render for CandidatePanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let Some(view) = snapshot(&self.state) else {
            return div().into_any_element();
        };
        let ps = view.page_size.max(1);
        let start = view.page * ps;
        let items: Vec<(usize, String, bool)> = view
            .candidates
            .iter()
            .enumerate()
            .skip(start)
            .take(ps)
            .map(|(i, text)| (i - start + 1, text.clone(), i == view.highlight))
            .collect();
        div()
            .font_family("Microsoft YaHei")
            .flex()
            .flex_col()
            .w(px(WIDTH))
            .bg(rgb(0xf7f7f9))
            .border_1()
            .border_color(rgb(0xd9d9e0))
            .rounded_lg()
            .shadow_lg()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .h(px(PREEDIT_H))
                    .px_3()
                    .items_center()
                    .text_size(px(14.0))
                    .text_color(rgb(0x50505a))
                    .child(SharedString::from(view.preedit)),
            )
            .children((!items.is_empty()).then(|| {
                div()
                    .flex()
                    .flex_wrap()
                    .px_2()
                    .pb_2()
                    .gap_1()
                    .children(items.into_iter().map(|(n, text, hl)| {
                        div()
                            .flex()
                            .items_baseline()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_size(px(17.0))
                            .map(|el| {
                                if hl {
                                    el.bg(rgb(0x2f6fed)).text_color(rgb(0xffffff))
                                } else {
                                    el.text_color(rgb(0x20202a))
                                }
                            })
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(if hl { rgb(0xc8d8ff) } else { rgb(0x90909a) })
                                    .child(format!("{n}.")),
                            )
                            .child(SharedString::from(text))
                    }))
            }))
            .into_any_element()
    }
}
