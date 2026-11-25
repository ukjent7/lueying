//! 候选窗与模式徽标共用的显示器摆位、DPI 与会话快照助手。
//!
//! 显示器查询走 `rune::displays()` 与 `Display::work_area()`;只有
//! 无窗口启动兜底和前台全屏检测保留 Win32。

use lueying_protocol::SessionView;
use lueying_render::panel::{self, GridItem, Item, Layout, Measure, Metrics, PanelSpec};
use lueying_server::config::{UiFont, UiLayout};
use rune::{Display, displays};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId,
    IsWindowVisible, IsZoomed, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

thread_local! {
    /// Rune 的显示器查询是一次系统询问;同一帧/同一轮摆位会多次用到,
    /// 短缓存把重复查询压成一次。显示器热插拔最迟半秒后可见。
    static DISPLAY_CACHE: RefCell<Option<(Instant, Vec<Display>)>> = const { RefCell::new(None) };
}

fn cached_displays() -> Vec<Display> {
    const TTL: Duration = Duration::from_millis(500);
    DISPLAY_CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((read_at, list)) = slot.as_ref()
            && read_at.elapsed() < TTL
        {
            return list.clone();
        }
        let list = displays();
        if !list.is_empty() {
            *slot = Some((Instant::now(), list.clone()));
        }
        list
    })
}

/// 虚拟桌面左缘之外的停靠点。固定负坐标会落在左侧副屏上,露出空白候选窗。
pub(super) fn park_point() -> (i32, i32) {
    let all = cached_displays();
    if let Some(left) = all.iter().map(|display| display.position.0).min() {
        let top = all
            .iter()
            .map(|display| display.position.1)
            .min()
            .unwrap_or_default();
        return (left.saturating_sub(PARK_GAP), top);
    }
    // 启动第一帧前 Rune 还没有窗口可读显示器时的兜底。
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    (left.saturating_sub(PARK_GAP), top)
}

/// 光标所在显示器;落在显示器之间的空隙时取最近的一块。
pub(super) fn display_at(x: i32, y: i32) -> Option<Display> {
    let all = cached_displays();
    all.iter()
        .min_by_key(|display| display_distance(display, x, y))
        .cloned()
}

fn display_distance(display: &Display, x: i32, y: i32) -> i64 {
    let (left, top) = display.position;
    let (width, height) = display.size;
    let left = i64::from(left);
    let top = i64::from(top);
    let right = left + i64::from(width);
    let bottom = top + i64::from(height);
    let x = i64::from(x);
    let y = i64::from(y);
    let dx = if x < left {
        left - x
    } else if x >= right {
        x - (right - 1)
    } else {
        0
    };
    let dy = if y < top {
        top - y
    } else if y >= bottom {
        y - (bottom - 1)
    } else {
        0
    };
    dx * dx + dy * dy
}

fn primary_display() -> Option<Display> {
    let all = cached_displays();
    all.iter()
        .find(|display| display.primary)
        .or_else(|| all.first())
        .cloned()
}

fn work_area_of(display: &Display) -> RECT {
    let ((x, y), (width, height)) = display.work_area();
    RECT {
        left: x,
        top: y,
        right: x.saturating_add(width as i32),
        bottom: y.saturating_add(height as i32),
    }
}

/// 主屏工作区;没有 Rune Display 时退回 Win32。
pub(super) fn primary_work_area() -> RECT {
    if let Some(display) = primary_display() {
        return work_area_of(&display);
    }
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    win32_monitor_info(monitor).map_or(
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
    display_at(x, y).map_or(1.0, |display| display.scale as f32)
}

/// 光标所在显示器的工作区(物理像素);取不到时给主屏。
pub(super) fn work_area_at(x: i32, y: i32) -> RECT {
    display_at(x, y).map_or_else(primary_work_area, |display| work_area_of(&display))
}

fn win32_monitor_info(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<MONITORINFO> {
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

/// 主屏原点与缩放;placement 的 Rect 相对主屏原点。
pub(super) fn primary_basis() -> ((i32, i32), f32) {
    if let Some(display) = primary_display() {
        return (display.position, display.scale as f32);
    }
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).unwrap_or_default(),
        ..Default::default()
    };
    let origin = if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        (info.rcMonitor.left, info.rcMonitor.top)
    } else {
        (0, 0)
    };
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    let scale = if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
        .is_ok()
        && dpi_x > 0
    {
        dpi_x as f32 / 96.0
    } else {
        1.0
    };
    (origin, scale)
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
    let Some(display) = display_at(x, y) else {
        return false;
    };
    let (left, top) = display.position;
    let (width, height) = display.size;
    rect.left <= left
        && rect.top <= top
        && rect.right >= left.saturating_add(width as i32)
        && rect.bottom >= top.saturating_add(height as i32)
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
    // 只有横排固定条按自然宽计算本页容量;竖排直接跟引擎页大小,
    // 不必为一次用不上的容量遍历逐条测宽。
    let page_size = if horizontal {
        panel::strip_capacity(
            view.candidates
                .iter()
                .map(|text| measure.width(text, metrics.text)),
            metrics,
        )
    } else {
        view.page_size
    };
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
