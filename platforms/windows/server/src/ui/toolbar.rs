//! 输入法工具栏:一排常用按钮的悬浮条,默认在屏幕右下角,可以拖动。
//!
//! 画法与模式徽标一样:分层窗口 + Direct2D + UpdateLayeredWindow。
//! 窗口不带穿透样式,所以能接收鼠标;按钮的动作复用服务端已有的请求处理,
//! 不另写一套。位置与大小的模型在 `lueying_render::toolbar`,这里只做窗口与输入。

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lueying_protocol::Request;
use lueying_render::paint::{Canvas, Color};
use lueying_render::panel::Rect;
use lueying_render::toolbar::{Action, Button, Toolbar};
use lueying_server::theme;
use lueying_server::ui_surface::Surface;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

use crate::config;
use crate::imp::{State, handle};
use crate::ui::place;

/// 鼠标左键按下时的按住标志(`MK_LBUTTON`)。
const MK_LBUTTON: usize = 0x0001;
/// 重绘之间的等待;工具栏静止时只做必要的重绘。
const IDLE_TICK: Duration = Duration::from_millis(16);
/// 圆角半径(倍率 1.0)。
const RADIUS: f32 = 9.0;
/// 字号(倍率 1.0)。
const FONT: f32 = 14.0;

/// 窗口线程独占的状态:工具栏模型、窗口左上角的屏幕坐标、按钮交互。
struct Ctx {
    state: Arc<State>,
    hwnd: HWND,
    bar: Toolbar,
    screen: Rect,
    /// 窗口左上角(屏幕坐标);鼠标坐标要加上它。
    origin: (f32, f32),
    /// 按下时落在哪个按钮上。
    pressed: Option<Button>,
    /// 按下后是否真的拖动过;拖动过就不算点击。
    moved: bool,
    dirty: bool,
}

thread_local! {
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

/// 起工具栏线程。工具栏独立于候选窗,候选窗的 GPU 管线起不来时照样显示。
pub(super) fn spawn(state: Arc<State>) {
    let spawned = std::thread::Builder::new()
        .name("toolbar".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| toolbar_loop(state)))
                .is_err()
            {
                println!("工具栏线程崩溃,工具栏不再显示");
            }
        });
    if let Err(e) = spawned {
        println!("工具栏线程启动失败: {e}");
    }
}

fn toolbar_loop(state: Arc<State>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let screen = to_rect(place::primary_work_area());
    let bar = Toolbar::at_default(screen);
    let hwnd = match create_window(&bar, screen) {
        Ok(hwnd) => hwnd,
        Err(e) => {
            println!("工具栏建窗失败: {e}");
            return;
        }
    };
    let mut surface = match Surface::new() {
        Ok(surface) => surface,
        Err(e) => {
            println!("工具栏建表面失败: {e}");
            return;
        }
    };
    CTX.with(|cell| {
        *cell.borrow_mut() = Some(Ctx {
            state,
            hwnd,
            bar,
            screen,
            origin: (0.0, 0.0),
            pressed: None,
            moved: false,
            dirty: true,
        });
    });
    loop {
        pump_messages();
        let dirty = CTX.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .map(|ctx| std::mem::take(&mut ctx.dirty))
                .unwrap_or(false)
        });
        if dirty {
            CTX.with(|cell| {
                if let Some(ctx) = cell.borrow_mut().as_mut() {
                    redraw(ctx, &mut surface);
                }
            });
        }
        std::thread::sleep(IDLE_TICK);
    }
}

fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 重画并上屏:按当前模型定窗口位置与大小;不可见时隐藏窗口。
fn redraw(ctx: &mut Ctx, surface: &mut Surface) {
    if !ctx.bar.visible {
        unsafe {
            let _ = ShowWindow(ctx.hwnd, SW_HIDE);
        }
        return;
    }
    let stage = ctx.bar.stage(ctx.screen);
    ctx.origin = (stage.left, stage.top);
    let (w, h) = (
        (stage.right - stage.left).max(1.0),
        (stage.bottom - stage.top).max(1.0),
    );
    let scale = ctx.bar.scale;
    let palette = theme::palette(theme::colors_with(
        theme::Appearance::of_system(),
        place::ui_options(&ctx.state).2,
    ));
    let local = |r: Rect| Rect {
        left: r.left - stage.left,
        top: r.top - stage.top,
        right: r.right - stage.left,
        bottom: r.bottom - stage.top,
    };
    let bar_local = local(ctx.bar.rect());
    let labels = button_labels(&ctx.state);
    match surface.begin(w as u32, h as u32) {
        Ok(canvas) => {
            canvas.fill_rrect(bar_local, RADIUS * scale, Color::opaque(palette.bg));
            canvas.stroke_rrect(bar_local, RADIUS * scale, 1.0, Color::opaque(palette.border));
            for (button, rect) in ctx.bar.buttons() {
                let text = labels
                    .iter()
                    .find(|(b, _)| *b == button)
                    .map(|(_, text)| *text)
                    .unwrap_or("");
                canvas.text_centered(
                    text,
                    local(rect),
                    FONT * scale,
                    Color::opaque(palette.text),
                );
            }
        }
        Err(e) => println!("工具栏表面失败: {e}"),
    }
    if let Err(e) = surface.present(
        ctx.hwnd,
        stage.left as i32,
        stage.top as i32,
        w as i32,
        h as i32,
    ) {
        println!("工具栏上屏失败: {e}");
    } else {
        unsafe {
            let _ = ShowWindow(ctx.hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

/// 每个按钮当前显示的字:随状态变,例如中/英、全/半。
fn button_labels(state: &State) -> Vec<(Button, &'static str)> {
    let chinese = state.chinese.load(Ordering::Relaxed);
    let fullwidth = state.fullwidth.load(Ordering::Relaxed);
    let (punct, traditional) = {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        (cfg.chinese_punct, cfg.traditional)
    };
    vec![
        (Button::Mode, if chinese { "中" } else { "英" }),
        (Button::FullWidth, if fullwidth { "全" } else { "半" }),
        (Button::Punct, if punct { "，" } else { "," }),
        (Button::Script, if traditional { "繁" } else { "简" }),
        (Button::Settings, "设"),
        (Button::Hide, "×"),
    ]
}

fn point(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    (x as f32, y as f32)
}

fn on_down(lparam: LPARAM) {
    let (cx, cy) = point(lparam);
    CTX.with(|cell| {
        if let Some(ctx) = cell.borrow_mut().as_mut() {
            let (sx, sy) = (cx + ctx.origin.0, cy + ctx.origin.1);
            ctx.pressed = ctx.bar.hit(sx, sy);
            ctx.moved = false;
            ctx.bar.press(sx, sy);
        }
    });
}

fn on_move(wparam: WPARAM, lparam: LPARAM) {
    if wparam.0 & MK_LBUTTON == 0 {
        return;
    }
    let (cx, cy) = point(lparam);
    CTX.with(|cell| {
        if let Some(ctx) = cell.borrow_mut().as_mut() {
            let (sx, sy) = (cx + ctx.origin.0, cy + ctx.origin.1);
            let before = (ctx.bar.x, ctx.bar.y);
            ctx.bar.drag(sx, sy, ctx.screen);
            if (ctx.bar.x, ctx.bar.y) != before {
                ctx.moved = true;
                ctx.dirty = true;
            }
        }
    });
}

fn on_up(lparam: LPARAM) {
    let (cx, cy) = point(lparam);
    let action = CTX.with(|cell| {
        let mut guard = cell.borrow_mut();
        let ctx = guard.as_mut()?;
        let (sx, sy) = (cx + ctx.origin.0, cy + ctx.origin.1);
        ctx.bar.release();
        let pressed = ctx.pressed.take();
        let released_on = ctx.bar.hit(sx, sy);
        ctx.dirty = true;
        if ctx.moved {
            return None;
        }
        match (pressed, released_on) {
            (Some(p), Some(r)) if p == r => Some((p.action(), ctx.state.clone())),
            _ => None,
        }
    });
    if let Some((action, state)) = action {
        run_action(&state, action);
    }
}

/// 按钮的动作。模式、全半角、标点复用服务端的请求处理;简繁写配置;设置与收起直接处理。
fn run_action(state: &Arc<State>, action: Action) {
    match action {
        Action::ToggleMode => {
            let session = state.active.load(Ordering::Relaxed);
            if session != 0 {
                let _ = handle(state, Request::ToggleMode { session });
            }
        }
        Action::ToggleFullWidth => {
            let session = state.active.load(Ordering::Relaxed);
            if session != 0 {
                let _ = handle(state, Request::ToggleFullwidth { session });
            }
        }
        Action::TogglePunct => {
            let _ = handle(state, Request::TogglePunct);
        }
        Action::ToggleScript => toggle_traditional(state),
        Action::OpenSettings => open_settings(),
        Action::Hide => CTX.with(|cell| {
            if let Some(ctx) = cell.borrow_mut().as_mut() {
                ctx.bar.visible = false;
                ctx.dirty = true;
            }
        }),
    }
    CTX.with(|cell| {
        if let Some(ctx) = cell.borrow_mut().as_mut() {
            ctx.dirty = true;
        }
    });
}

/// 简繁切换:写进配置并保存,然后应用。和标点切换走同一条路。
fn toggle_traditional(state: &Arc<State>) {
    let cfg = {
        let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
        cfg.traditional = !cfg.traditional;
        cfg.clone()
    };
    match &state.config_path {
        Some(path) => {
            if let Err(e) = config::save(path, &cfg) {
                println!("保存配置 {} 失败: {e}", path.display());
            }
        }
        None => println!("切换简繁未保存: 没有配置路径"),
    }
    crate::imp::apply_config(state, cfg);
}

/// 打开设置窗口:与语言栏的「设置」一样启动同目录的 `lueying-settings.exe`。
fn open_settings() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    else {
        return;
    };
    if let Err(e) = std::process::Command::new(dir.join("lueying-settings.exe")).spawn() {
        println!("启动设置失败: {e}");
    }
}

fn to_rect(r: RECT) -> Rect {
    Rect {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}

/// 无焦点置顶、接收鼠标的分层窗口。
fn create_window(bar: &Toolbar, screen: Rect) -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let stage = bar.stage(screen);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("lueying-toolbar"),
            w!("lueying-toolbar"),
            WS_POPUP,
            stage.left as i32,
            stage.top as i32,
            (stage.right - stage.left) as i32,
            (stage.bottom - stage.top) as i32,
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
            lpszClassName: w!("lueying-toolbar"),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW 失败".into());
        }
    });
    result
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => on_down(lparam),
        WM_MOUSEMOVE => on_move(wparam, lparam),
        WM_LBUTTONUP => on_up(lparam),
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
