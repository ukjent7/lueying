//! 模式徽标:Win32 分层窗口 + Skia 光栅绘制。
//!
//! 徽标与候选窗各走各的渲染路径:候选窗在 Ganesh-D3D12 管线里,徽标走 CPU
//! 光栅——徽标得在没有硬件 D3D12 的机器上也看得见,分层窗口不依赖 GPU。
//! 画面是预乘 alpha 的位图,靠 UpdateLayeredWindow 上屏,圆角、描边与淡入
//! 淡出都是逐像素的。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lueying_render::badge;
use lueying_render::panel::Palette;
use lueying_server::blit::Layer;
use lueying_server::theme;
use skia_safe::{Font, Surface};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use crate::imp::State;
use crate::ui_place;

/// 徽标与光标之间的距离(逻辑像素)。
const GAP: f32 = 4.0;
/// 首次出现淡入:首帧就有 70% 不透明度,120ms 内到 100%。
const HUD_FADE_IN: Duration = Duration::from_millis(120);
const HUD_START_OPACITY: f32 = 0.7;
/// 显示 600ms 后开始 180ms 淡出。
const HUD_SHOW: Duration = Duration::from_millis(600);
const HUD_FADE: Duration = Duration::from_millis(180);
const HUD_TOTAL: Duration = Duration::from_millis(780);
/// 轮询间隔;徽标只在切换后出现,15ms 足够跟手。
const POLL: Duration = Duration::from_millis(15);

/// 当前这份徽标:显示哪个字、贴哪个光标、从何时开始计时。
#[derive(Clone)]
struct HudView {
    label: &'static str,
    caret: Option<(i32, i32, i32, i32)>,
    at: Instant,
}

/// 起徽标线程;徽标独立于候选窗,GPU 不可用时照样显示。
pub(super) fn spawn(state: Arc<State>) {
    let spawned = std::thread::Builder::new()
        .name("mode-hud".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hud_loop(state))).is_err() {
                println!("模式徽标线程崩溃,中英切换不再提示");
            }
        });
    if let Err(e) = spawned {
        println!("模式徽标线程启动失败: {e}");
    }
}

/// 徽标线程:轮询切换序号,到期前画一帧,到期后把窗口挪出屏幕。
fn hud_loop(state: Arc<State>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            println!("模式徽标建窗失败: {e}");
            return;
        }
    };
    let mut layer = match Layer::new() {
        Ok(layer) => layer,
        Err(e) => {
            println!("模式徽标建位图失败: {e}");
            return;
        }
    };
    let font = crate::ui_skia::panel_font();
    let mut serial = 0u64;
    let mut current: Option<HudView> = None;
    // 首帧上屏后再显示,避免从停靠点挪到光标处时闪一帧旧徽标。
    let mut visible = false;
    // 外观、主题色与动效开关按事件取一次:系统主题切得很慢,每帧读注册表
    // 与系统参数纯属浪费。
    let mut appearance = theme::Appearance::of_system();
    let mut accent = accent_of(&state);
    let mut reduce = false;
    loop {
        // 徽标不吃输入,消息排干净就行。
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let now = Instant::now();
        let next = state.hud_serial.load(Ordering::Relaxed);
        if next != serial {
            serial = next;
            current = hud_view(&state);
            appearance = theme::Appearance::of_system();
            accent = accent_of(&state);
            reduce = crate::ui_skia::reduce_motion();
        }
        let expired = current
            .as_ref()
            .is_some_and(|view| now.saturating_duration_since(view.at) >= HUD_TOTAL);
        if expired {
            current = None;
        }
        match current.clone() {
            Some(view) => {
                if let Some((x, y, w, h)) = place(&state, &view) {
                    let scale = w as f32 / badge::WIDTH;
                    let palette = theme::palette(theme::colors_with(appearance, accent));
                    let opacity = opacity(&view, now, reduce);
                    match layer.surface(w as u32, h as u32) {
                        Ok(surface) => {
                            draw_badge(
                                surface,
                                &view,
                                font.as_ref(),
                                &palette,
                                appearance,
                                opacity,
                                scale,
                            );
                        }
                        Err(e) => println!("模式徽标表面失败: {e}"),
                    }
                    if let Err(e) = layer.present(hwnd, x, y, w, h) {
                        println!("模式徽标上屏失败: {e}");
                    } else if !visible {
                        unsafe {
                            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        }
                        visible = true;
                    }
                } else {
                    park(hwnd, &mut visible);
                }
            }
            None => park(hwnd, &mut visible),
        }
        std::thread::sleep(POLL);
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
    ui_place::ui_options(state).2
}

/// 徽标摆位:平时贴在光标下方;候选窗在同一侧时翻到另一侧,两者不互相遮挡。
/// 返回 (x, y, 宽, 高)(物理像素)。
fn place(state: &State, view: &HudView) -> Option<(i32, i32, i32, i32)> {
    let caret = view
        .caret
        .map(|(l, t, r, b)| (l.min(r), t.min(b), l.max(r), b.max(t)));
    let host_pid = (state.active.load(Ordering::Relaxed) >> 32) as u32;
    let (anchor_x, anchor_y, work) = match caret {
        Some((l, _, r, b)) => (l + (r - l) / 2, b, ui_place::work_area_at(l, b)),
        None => {
            let work = ui_place::primary_work_area();
            (
                work.left + (work.right - work.left) / 2,
                work.top + (work.bottom - work.top) * 2 / 3,
                work,
            )
        }
    };
    if ui_place::foreground_fullscreen_at(anchor_x, anchor_y, host_pid) {
        return None;
    }
    let scale = ui_place::scale_at(anchor_x, anchor_y);
    let w = (badge::WIDTH * scale) as i32;
    let h = (badge::HEIGHT * scale) as i32;
    let gap = (GAP * scale) as i32;
    // 候选窗在哪一侧,徽标就去另一侧。
    let composing = ui_place::in_composition(state);
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

/// 画一帧徽标:圆角卡片 + 描边 + 居中的字。
fn draw_badge(
    surface: &mut Surface,
    view: &HudView,
    font: Option<&Font>,
    palette: &Palette,
    appearance: theme::Appearance,
    opacity: f32,
    scale: f32,
) {
    let (width, height) = (surface.width() as f32, surface.height() as f32);
    let canvas = surface.canvas();
    canvas.clear(skia_safe::Color::TRANSPARENT);
    // 淡入淡出作用整窗;圆角之外的像素保持透明。
    if opacity < 1.0 {
        canvas.save_layer_alpha_f(
            skia_safe::Rect::from_wh(width, height),
            opacity.clamp(0.0, 1.0),
        );
    }
    let high_contrast = matches!(appearance, theme::Appearance::HighContrast);
    badge::draw(
        canvas,
        &badge::BadgeSpec {
            label: view.label,
            palette,
            font,
            width,
            height,
            scale,
            shadow: !high_contrast,
        },
    );
    if opacity < 1.0 {
        canvas.restore();
    }
}

/// 无焦点置顶的分层窗口:画面只由 UpdateLayeredWindow 给,系统不画背景。
fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let (park_x, park_y) = ui_place::park_point();
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
    let (x, y) = ui_place::park_point();
    ui_place::park(hwnd, x, y);
}
