//! 模式徽标:GPUI 无焦点置顶浮层。
//!
//! 候选窗已经迁到 Skia 管线(`ui_skia`:独立线程 + DirectComposition +
//! Ganesh-D3D12),本模块只留中英切换徽标。gpui 未暴露窗口移动/置顶,经
//! raw-window-handle 拿 HWND 走 SetWindowPos;空闲时把窗口挪出屏幕而不是
//! 隐藏,避免和 gpui 自己的可见性状态打架。

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;

use crate::imp::State;
use gpui::{
    App, Bounds, Point, Window, WindowBounds, WindowKind, WindowOptions, prelude::*, px, size,
};
use gpui_platform::application;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SWP_NOACTIVATE, SetWindowLongPtrW, SetWindowPos,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};

#[path = "ui_hud.rs"]
mod hud;

use hud::ModeHudPanel;

/// 中英切换徽标:紧凑圆角卡片,120ms 淡入,600ms 后开始 180ms 淡出。
pub(super) const HUD_W: f32 = 46.0;
pub(super) const HUD_H: f32 = 32.0;
pub(super) const HUD_FADE_IN: Duration = Duration::from_millis(120);
pub(super) const HUD_START_OPACITY: f32 = 0.7;
pub(super) const HUD_SHOW: Duration = Duration::from_millis(600);
pub(super) const HUD_FADE: Duration = Duration::from_millis(180);
pub(super) const HUD_TOTAL: Duration = Duration::from_millis(780);

/// 起候选窗(Skia 线程)与模式徽标(GPUI 事件循环)。
pub(super) fn run(state: Arc<State>) {
    // 候选窗:独立线程自建消息泵,不走 gpui 事件循环。
    crate::ui_skia::spawn(Arc::clone(&state));
    application().run(move |cx: &mut App| {
        let (park_x, park_y) = crate::ui_place::park_point();
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

/// 从 gpui 窗口取原生 HWND;窗口系统不是 Win32 时返回 None。
pub(super) fn window_hwnd(window: &Window) -> Option<HWND> {
    let raw = <Window as HasWindowHandle>::window_handle(window).ok()?.as_raw();
    let RawWindowHandle::Win32(handle) = raw else {
        return None;
    };
    Some(HWND(handle.hwnd.get() as *mut c_void))
}

/// 无焦点置顶工具窗样式;`click_through` 时再让鼠标穿透到宿主。
pub(super) fn style_popup(hwnd: HWND, styled: &Cell<bool>, click_through: bool) {
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
