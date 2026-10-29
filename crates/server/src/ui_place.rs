//! 候选窗与模式徽标共用的 Win32 摆位、DPI 与会话快照助手。
//!
//! GPUI 路径(`ui.rs`)与 Skia 路径(`ui_skia.rs`)共用同一套停靠点、
//! 逐显示器 DPI、工作区夹取与全屏避让;"画什么"由会话快照决定。

use std::sync::atomic::Ordering;

use lueying_protocol::SessionView;
use lueying_render::panel::{self, Item, Layout, Metrics, PanelSpec, PreeditBar};
use lueying_server::config::{UiFont, UiLayout};
use skia_safe::Font;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId, HWND_TOPMOST,
    IsWindowVisible, IsZoomed, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOSIZE,
    SetWindowPos,
};

use crate::imp::State;

/// 空闲停靠位到虚拟桌面左缘的距离。按虚拟桌面算,左侧副屏也不会露出空窗。
pub(super) const PARK_GAP: i32 = 2000;
/// 候选窗与光标之间留一点呼吸,不贴着字形下缘。
pub(super) const CARET_GAP: f32 = 4.0;

/// 候选窗要画的一帧:会话视图加隐私标记。
pub(super) struct Snapshot {
    pub(super) view: SessionView,
    pub(super) private: bool,
}

/// 激活会话且在组合中才有窗口内容。宿主接管候选绘制时让位。
pub(super) fn snapshot(state: &State) -> Option<Snapshot> {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let session = sessions.get(&state.active.load(Ordering::Relaxed))?;
    (!session.host_draws && session.engine.composition().is_some()).then(|| Snapshot {
        view: session.view.clone(),
        private: session.private,
    })
}

/// 读取候选窗显示设置(排列、字号、主题色)。
pub(super) fn ui_options(state: &State) -> (UiLayout, UiFont, usize) {
    let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
    (cfg.ui_layout, cfg.ui_font, cfg.ui_accent)
}

/// 组合仍在进行;淡出窗口里的旧候选不能响应鼠标。宿主接管候选绘制时同样算组合中。
pub(super) fn in_composition(state: &State) -> bool {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    sessions
        .get(&state.active.load(Ordering::Relaxed))
        .is_some_and(|s| s.engine.composition().is_some())
}

/// 服务端候选窗在显示;宿主接管绘制时让位。
pub(super) fn composing(state: &State) -> bool {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    sessions
        .get(&state.active.load(Ordering::Relaxed))
        .is_some_and(|s| !s.host_draws && s.engine.composition().is_some())
}

/// 把窗口挪出虚拟桌面并收起,避免在任务栏/Alt+Tab 里露出空窗。
pub(super) fn park(hwnd: HWND, x: i32, y: i32) {
    unsafe {
        let _ = SetWindowPos(
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

/// 虚拟桌面左缘之外的停靠点。固定负坐标会落在左侧副屏上,露出空白候选窗。
pub(super) fn park_point() -> (i32, i32) {
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    (left.saturating_sub(PARK_GAP), top)
}

pub(super) fn monitor_at(x: i32, y: i32) -> HMONITOR {
    unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) }
}

pub(super) fn monitor_info_at(x: i32, y: i32) -> Option<MONITORINFO> {
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
pub(super) fn primary_work_area() -> RECT {
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
pub(super) fn scale_at(x: i32, y: i32) -> f32 {
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
pub(super) fn work_area_at(x: i32, y: i32) -> RECT {
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
pub(super) fn foreground_fullscreen_at(x: i32, y: i32, host_pid: u32) -> bool {
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

/// 会话视图转面板输入;页切片、选中态与页码标签与候选窗同一套语义。
pub(super) fn spec_of(view: &SessionView, layout: UiLayout) -> PanelSpec {
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

/// 全组合候选都参与估宽;翻页时窗口宽度保持一致,不会来回跳。
pub(super) fn measure_width(
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
