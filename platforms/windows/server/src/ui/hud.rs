//! 模式徽标:Win32 分层窗口 + Direct2D 绘制。
//!
//! 徽标与候选窗各走各的渲染路径:候选窗在 DirectComposition 管线里,徽标
//! 画在 Direct2D 兼容的 DC 表面上——分层窗口不依赖 GPU,候选窗管线起不来
//! 时徽标也照常显示。画面是预乘 alpha 的位图,靠 UpdateLayeredWindow 上屏,
//! 圆角、描边与淡入淡出都由绘制层给出。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lueying_render::badge;
use lueying_render::paint::Canvas;
use lueying_render::panel::Palette;
use lueying_server::theme;
use lueying_server::ui_surface::Surface;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};
use windows::core::w;

use crate::imp::State;
use crate::ui::place;

/// 徽标与光标之间的距离(逻辑像素)。
const GAP: f32 = 4.0;
/// 首次出现淡入:首帧就有 70% 不透明度,120ms 内到 100%。
const HUD_FADE_IN: Duration = Duration::from_millis(120);
const HUD_START_OPACITY: f32 = 0.7;
/// 显示 600ms 后开始 180ms 淡出。
const HUD_SHOW: Duration = Duration::from_millis(600);
const HUD_FADE: Duration = Duration::from_millis(180);
const HUD_TOTAL: Duration = Duration::from_millis(780);
/// 淡入淡出期间的重绘节奏。
const HUD_TICK: Duration = Duration::from_millis(15);

/// 当前这份徽标:显示哪个字、贴哪个光标、从何时开始计时。
#[derive(Clone)]
struct HudView {
    label: &'static str,
    caret: Option<(i32, i32, i32, i32)>,
    at: Instant,
}

/// 模式徽标:分层窗口与它的表面,以及当前这份徽标的显示状态。
pub(super) struct Badge {
    hwnd: HWND,
    surface: Surface,
    /// 上次看到的切换序号;没变就不重读徽标与外观。
    serial: u64,
    current: Option<HudView>,
    /// 首帧上屏后才显示,避免从停靠点挪到光标处时闪一帧旧徽标。
    visible: bool,
    // 外观、主题色与动效开关按切换取一次:系统主题切得很慢,每帧读注册表
    // 与系统参数纯属浪费。
    appearance: theme::Appearance,
    accent: usize,
    reduce: bool,
    /// 下一次该醒来的时间;停靠时为 `None`。
    next: Option<Duration>,
}

/// 建徽标窗口。建不出来只写日志并返回 `None`,中英切换照常生效。
pub(super) fn init(state: &Arc<State>) -> Option<Badge> {
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            println!("模式徽标建窗失败: {e}");
            return None;
        }
    };
    let surface = match Surface::new() {
        Ok(surface) => surface,
        Err(e) => {
            println!("模式徽标建表面失败: {e}");
            return None;
        }
    };
    // 先挪到停靠点:之后只在切换时才动窗口。
    let mut visible = false;
    park(hwnd, &mut visible);
    Some(Badge {
        hwnd,
        surface,
        serial: 0,
        current: None,
        visible,
        appearance: theme::Appearance::of_system(),
        accent: accent_of(state),
        reduce: false,
        next: None,
    })
}

impl Badge {
    /// 切换序号变了就重读徽标;可见期间按淡入/保持/淡出分段出帧,到期后停靠。
    pub(super) fn tick(&mut self, state: &State) {
        let serial = state.hud_serial.load(Ordering::Acquire);
        // 停靠且没有新的切换:什么都不用做,也不挪窗口。
        if serial == self.serial && self.current.is_none() {
            self.next = None;
            return;
        }
        let now = Instant::now();
        if serial != self.serial {
            self.serial = serial;
            self.current = hud_view(state);
            self.appearance = theme::Appearance::of_system();
            self.accent = accent_of(state);
            self.reduce = crate::ui::d2d::reduce_motion();
        }
        if self
            .current
            .as_ref()
            .is_some_and(|view| now.saturating_duration_since(view.at) >= HUD_TOTAL)
        {
            self.current = None;
        }
        match self.current.clone() {
            Some(view) => self.draw(state, &view, now),
            None => park(self.hwnd, &mut self.visible),
        }
        self.next = next_wait(self.current.as_ref(), Instant::now(), self.reduce);
    }

    /// 下一个动画节点;停靠时为空。
    pub(super) fn timeout(&self) -> Option<Duration> {
        self.next
    }

    fn draw(&mut self, state: &State, view: &HudView, now: Instant) {
        let Some((x, y, w, h)) = place(state, view) else {
            park(self.hwnd, &mut self.visible);
            return;
        };
        let scale = w as f32 / badge::WIDTH;
        let palette = theme::palette(theme::colors_with(self.appearance, self.accent));
        let opacity = opacity(view, now, self.reduce);
        match self.surface.begin(w as u32, h as u32) {
            Ok(canvas) => draw_badge(
                canvas,
                view,
                &palette,
                self.appearance,
                opacity,
                scale,
                (w as f32, h as f32),
            ),
            Err(e) => println!("模式徽标表面失败: {e}"),
        }
        if let Err(e) = self.surface.present(self.hwnd, x, y, w, h) {
            println!("模式徽标上屏失败: {e}");
        } else if !self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.visible = true;
        }
    }
}

/// 下一次该醒来的时间:停靠时为 None,可见时按淡入/保持/淡出分段。
fn next_wait(current: Option<&HudView>, now: Instant, reduce: bool) -> Option<Duration> {
    let view = current?;
    let elapsed = now.saturating_duration_since(view.at);
    if elapsed >= HUD_TOTAL {
        return Some(Duration::ZERO);
    }
    if reduce {
        return Some(HUD_TOTAL - elapsed);
    }
    if elapsed < HUD_FADE_IN {
        Some(HUD_TICK.min(HUD_FADE_IN - elapsed))
    } else if elapsed < HUD_SHOW {
        Some(HUD_SHOW - elapsed)
    } else {
        Some(HUD_TICK.min(HUD_TOTAL - elapsed))
    }
}

/// 读当前这份徽标;没有就清空。
fn hud_view(state: &State) -> Option<HudView> {
    let source = state.mode_hud.lock().unwrap_or_else(|p| p.into_inner());
    source.as_ref().map(|hud| HudView {
        label: hud.label,
        caret: hud.caret,
        at: hud.at,
    })
}

/// 主题色下标。
fn accent_of(state: &State) -> usize {
    place::ui_options(state).2
}

/// 徽标摆位:平时贴在光标下方;候选窗在同一侧时翻到另一侧,两者不互相遮挡。
/// 返回 (x, y, 宽, 高)(物理像素)。
fn place(state: &State, view: &HudView) -> Option<(i32, i32, i32, i32)> {
    let caret = view
        .caret
        .map(|(l, t, r, b)| (l.min(r), t.min(b), l.max(r), b.max(t)));
    let host_pid = (state.active.load(Ordering::Relaxed) >> 32) as u32;
    let (anchor_x, anchor_y, work) = match caret {
        Some((l, _, r, b)) => (l + (r - l) / 2, b, place::work_area_at(l, b)),
        None => {
            let work = place::primary_work_area();
            (
                work.left + (work.right - work.left) / 2,
                work.top + (work.bottom - work.top) * 2 / 3,
                work,
            )
        }
    };
    if place::foreground_fullscreen_at(anchor_x, anchor_y, host_pid) {
        return None;
    }
    let scale = place::scale_at(anchor_x, anchor_y);
    let w = (badge::WIDTH * scale) as i32;
    let h = (badge::HEIGHT * scale) as i32;
    let gap = (GAP * scale) as i32;
    // 候选窗在哪一侧,徽标就去另一侧。
    let composing = place::in_composition(state);
    let candidate_above = state.candidate_above.load(Ordering::Relaxed);
    let prefer_below = !composing || !candidate_above;
    let x = (anchor_x - w / 2).clamp(work.left, (work.right - w).max(work.left));
    let y = (match caret {
        Some((_, t, _, b)) => {
            let below = b + gap;
            let above = t - h - gap;
            if prefer_below && below + h <= work.bottom {
                below
            } else if !prefer_below && above >= work.top {
                above
            } else if below + h <= work.bottom {
                below
            } else {
                above
            }
        }
        None => anchor_y - h / 2,
    })
    .clamp(work.top, (work.bottom - h).max(work.top));
    Some((x, y, w, h))
}

/// 当前该有多不透明:淡入、保持、淡出三段。
fn opacity(view: &HudView, now: Instant, reduce: bool) -> f32 {
    if reduce {
        return 1.0;
    }
    let elapsed = now.saturating_duration_since(view.at);
    if elapsed >= HUD_TOTAL {
        0.0
    } else if elapsed >= HUD_SHOW {
        ((HUD_TOTAL - elapsed).as_secs_f32() / HUD_FADE.as_secs_f32()).clamp(0.0, 1.0)
    } else if elapsed < HUD_FADE_IN {
        let t = elapsed.as_secs_f32() / HUD_FADE_IN.as_secs_f32();
        HUD_START_OPACITY + (1.0 - HUD_START_OPACITY) * t
    } else {
        1.0
    }
}

/// 画一帧徽标:圆角卡片 + 描边 + 居中的字;淡入淡出作用整枚徽标。
fn draw_badge(
    canvas: &mut dyn Canvas,
    view: &HudView,
    palette: &Palette,
    appearance: theme::Appearance,
    opacity: f32,
    scale: f32,
    (width, height): (f32, f32),
) {
    badge::draw(
        canvas,
        &badge::BadgeSpec {
            label: view.label,
            palette,
            width,
            height,
            scale,
            opacity,
            shadow: !matches!(appearance, theme::Appearance::HighContrast),
        },
    );
}

/// 无焦点置顶的分层窗口:画面只由 UpdateLayeredWindow 给,系统不画背景。
fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let (park_x, park_y) = place::park_point();
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_TRANSPARENT,
            w!("lueying-mode-hud"),
            w!("lueying-hud"),
            // 先隐藏建在停靠点;首帧经 UpdateLayeredWindow 上屏后再显示。
            WS_POPUP,
            park_x,
            park_y,
            badge::WIDTH as i32,
            badge::HEIGHT as i32,
            None,
            None,
            Some(HINSTANCE(hmodule.0)),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        Ok(hwnd)
    }
}

/// 窗口类进程内只注册一次。
fn ensure_class(hinstance: HINSTANCE) -> Result<(), String> {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: w!("lueying-mode-hud"),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW 失败".into());
        }
    });
    result
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 挪出虚拟桌面并隐藏;徽标不显示时停在这里,避免在任务栏里露脸。
fn park(hwnd: HWND, visible: &mut bool) {
    if *visible {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        *visible = false;
    }
    let (x, y) = place::park_point();
    place::park(hwnd, x, y);
}
