//! 候选窗与模式徽标共用的 Win32 摆位、DPI 与会话快照助手。
//!
//! 候选窗与模式徽标共用逐显示器 DPI、工作区夹取与全屏避让。

use lueying_protocol::SessionView;
use lueying_render::panel::{self, GridItem, Item, Layout, Measure, Metrics, PanelSpec};
use lueying_server::config::{UiFont, UiLayout};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, IsZoomed, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use std::sync::Arc;

use crate::imp::{self, State};

/// 空闲停靠位到虚拟桌面左缘的距离。按虚拟桌面算,左侧副屏也不会露出空窗。
pub(super) const PARK_GAP: i32 = 2000;
/// 候选窗与光标之间留一点呼吸,不贴着字形下缘。
pub(super) const CARET_GAP: f32 = 4.0;

/// 激活会话且在组合中才有窗口内容。宿主接管候选绘制时让位。
///
/// 只读已发布的视图:候选窗出帧不受任何会话的推理拖住;返回共享指针,
/// 渲染端与冻结内容同住一份,不再逐帧克隆候选列表。
pub(super) fn snapshot(state: &State) -> Option<Arc<imp::Published>> {
    let published = state.active_published()?;
    (!published.host_draws && published.composing).then_some(published)
}

/// 读取候选窗显示设置(排列、字号、主题色)。
pub(super) fn ui_options(state: &State) -> (UiLayout, UiFont, usize) {
    let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
    (cfg.ui_layout, cfg.ui_font, cfg.ui_accent)
}

/// 组合仍在进行;淡出窗口里的旧候选不能响应鼠标。
pub(super) fn in_composition(state: &State) -> bool {
    state
        .active_published()
        .is_some_and(|published| published.composing)
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

/// 会话视图转面板输入;展开态(view.expanded)只在横排生效:固定条是
/// 候选摘要,展开后 4×6 棋盘是一屏连续滚动的候选。固定条按内容自适应
/// 条数(1..=9),返回的条数同时喂给引擎当页大小,数字键才不会错位。
pub(super) fn spec_of(
    view: &SessionView,
    layout: UiLayout,
    metrics: &Metrics,
    measure: &dyn Measure,
) -> (PanelSpec, usize) {
    let horizontal = matches!(layout, UiLayout::Horizontal);
    let expand = view.expanded && horizontal;
    let len = view.candidates.len();
    let widths = view
        .candidates
        .iter()
        .map(|text| measure.width(text, metrics.text));
    let fit = panel::strip_capacity(widths, metrics);
    let page_size = if horizontal { fit } else { view.page_size };
    let page = panel::page_bounds(view.page, view.page_size, len);
    let (items, grid, scroll) = if horizontal && !expand {
        // 条只画当前页;页号来自引擎,翻页后候选必须跟着换。
        let items: Vec<Item> = page
            .clone()
            .map(|i| Item {
                text: view.candidates[i].clone(),
                hint: String::new(),
                selected: i == view.highlight,
            })
            .collect();
        (items, Vec::new(), None)
    } else if expand {
        // 棋盘是一屏滚动窗口:一屏 24 条,方向键到底行时整行滚动。
        let cap = panel::GRID_MAX;
        let start = panel::grid_clamp_scroll(view.grid_scroll, len);
        let end = (start + cap).min(len);
        let grid: Vec<GridItem> = (start..end)
            .map(|i| GridItem {
                text: view.candidates[i].clone(),
                index: i,
                selected: i == view.highlight,
            })
            .collect();
        (
            Vec::new(),
            grid,
            Some(panel::Scroll {
                offset: start,
                len,
                cap,
            }),
        )
    } else {
        let items = page
            .map(|i| Item {
                text: view.candidates[i].clone(),
                hint: view.hints.get(i).cloned().unwrap_or_default(),
                selected: i == view.highlight,
            })
            .collect();
        (items, Vec::new(), None)
    };
    let spec = PanelSpec {
        width: 0.0,
        // 摆位时按字号档 × DPI 补上;这里先置零。
        max_text_width: 0.0,
        expand,
        items,
        grid,
        scroll,
        layout: if horizontal {
            Layout::Wrap
        } else {
            Layout::Column
        },
    };
    (spec, page_size)
}

/// 全组合候选都参与估宽;翻页时窗口宽度保持一致,不会来回跳。
/// 直接按条目文本测宽,不为估宽克隆一份候选列表。
pub(super) fn measure_width(
    view: &SessionView,
    layout: UiLayout,
    metrics: &Metrics,
    measure: &dyn Measure,
) -> f32 {
    let (mut spec, _) = spec_of(view, layout, metrics, measure);
    // 横排/棋盘宽度固定;只有竖排要按全量候选的自然宽定窗口宽。
    if matches!(spec.layout, panel::Layout::Column) {
        spec.width = panel::measure_items_width(
            view.candidates.iter().enumerate().map(|(index, text)| {
                (
                    text.as_str(),
                    view.hints.get(index).map_or("", String::as_str),
                )
            }),
            metrics,
            measure,
        );
    }
    panel::panel_width(&spec, metrics, measure)
}
