//! 输入法工具栏:一排常用按钮的悬浮条,默认在屏幕右下角,可以拖动。
//!
//! 画法与模式徽标一样:分层窗口 + Direct2D + UpdateLayeredWindow。窗口不带穿透样式,
//! 所以能接收鼠标;按钮的动作复用服务端已有的请求处理。位置、大小与按钮的模型在
//! `lueying_render::toolbar`。

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lueying_protocol::Request;
use lueying_render::panel::Rect;
use lueying_render::toolbar::{self, Action, Button, Status, Toolbar};
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

use crate::imp::{State, handle};
use crate::ui::place;
use lueying_server::config;

/// 鼠标左键按住时的标志(`MK_LBUTTON`)。
const MK_LBUTTON: usize = 0x0001;
/// 轮询消息与脏标记的间隔;只在需要时重画。
const TICK: Duration = Duration::from_millis(33);

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
    let bar = saved_bar(&state, screen);
    let hwnd = match create_window(&bar) {
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
        std::thread::sleep(TICK);
    }
}

/// 上次保存的大小与位置;没有就用默认的右下角。位置夹在屏幕内。
fn saved_bar(state: &State, screen: Rect) -> Toolbar {
    let (scale, pos) = {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        (cfg.toolbar_scale, cfg.toolbar_pos)
    };
    let mut bar = Toolbar::at_default(screen).with_scale(scale, screen);
    if let Some((x, y)) = pos {
        bar.x = x as f32;
        bar.y = y as f32;
        bar = bar.clamped(screen);
    }
    bar
}

/// 拖动结束后把位置写回配置并保存,下次启动回到同一个地方。
fn save_position(state: &Arc<State>, x: i32, y: i32) {
    let (before, cfg) = {
        let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
        let before = cfg.clone();
        cfg.toolbar_pos = Some((x, y));
        (before, cfg.clone())
    };
    if let Some(path) = &state.config_path {
        if let Err(e) = config::save(path, &before, &cfg) {
            println!("保存工具栏位置 {} 失败: {e}", path.display());
        }
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

/// 重画并上屏:按模型定窗口位置与大小;不可见时隐藏窗口。
fn redraw(ctx: &mut Ctx, surface: &mut Surface) {
    if !ctx.bar.visible {
        unsafe {
            let _ = ShowWindow(ctx.hwnd, SW_HIDE);
        }
        return;
    }
    // 设置页改了大小:跟着配置走,位置保持不变。
    let configured = ctx
        .state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .toolbar_scale;
    if (configured - ctx.bar.scale).abs() > f32::EPSILON {
        ctx.bar = ctx.bar.with_scale(configured, ctx.screen);
    }
    let rect = ctx.bar.rect();
    ctx.origin = (rect.left, rect.top);
    let (w, h) = (
        (rect.right - rect.left).max(1.0),
        (rect.bottom - rect.top).max(1.0),
    );
    let palette = theme::palette(theme::colors_with(
        theme::Appearance::of_system(),
        place::ui_options(&ctx.state).2,
    ));
    let scene = toolbar::Scene {
        toolbar: &ctx.bar,
        origin: (rect.left, rect.top),
        palette: &palette,
        status: status_of(&ctx.state),
    };
    match surface.begin(w as u32, h as u32) {
        Ok(canvas) => toolbar::paint(canvas, &scene),
        Err(e) => println!("工具栏表面失败: {e}"),
    }
    if let Err(e) = surface.present(
        ctx.hwnd,
        rect.left as i32,
        rect.top as i32,
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

/// 按钮上的字随的状态:来自服务端的全局状态与配置。
fn status_of(state: &State) -> Status {
    let (punct, traditional) = {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        (cfg.chinese_punct, cfg.traditional)
    };
    Status {
        chinese: state.chinese.load(Ordering::Relaxed),
        fullwidth: state.fullwidth.load(Ordering::Relaxed),
        punct,
        traditional,
    }
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
    let mut dragged = None;
    let action = CTX.with(|cell| {
        let mut guard = cell.borrow_mut();
        let ctx = guard.as_mut()?;
        let (sx, sy) = (cx + ctx.origin.0, cy + ctx.origin.1);
        ctx.bar.release();
        let pressed = ctx.pressed.take();
        let released_on = ctx.bar.hit(sx, sy);
        ctx.dirty = true;
        if ctx.moved {
            dragged = Some((ctx.bar.x as i32, ctx.bar.y as i32, ctx.state.clone()));
            return None;
        }
        match (pressed, released_on) {
            (Some(p), Some(r)) if p == r => Some((p.action(), ctx.state.clone())),
            _ => None,
        }
    });
    if let Some((x, y, state)) = dragged {
        save_position(&state, x, y);
    }
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
    let (before, cfg) = {
        let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
        let before = cfg.clone();
        cfg.traditional = !cfg.traditional;
        (before, cfg.clone())
    };
    match &state.config_path {
        Some(path) => {
            if let Err(e) = config::save(path, &before, &cfg) {
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
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
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
fn create_window(bar: &Toolbar) -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let rect = bar.rect();
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("lueying-toolbar"),
            w!("lueying-toolbar"),
            WS_POPUP,
            rect.left as i32,
            rect.top as i32,
            (rect.right - rect.left) as i32,
            (rect.bottom - rect.top) as i32,
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

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => on_down(lparam),
        WM_MOUSEMOVE => on_move(wparam, lparam),
        WM_LBUTTONUP => on_up(lparam),
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
