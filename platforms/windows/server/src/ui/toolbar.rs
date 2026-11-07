//! 输入法工具栏:一排常用按钮的悬浮条,默认在屏幕右下角,可以拖动。舞台上可以有角色表演。
//!
//! 画法与模式徽标一样:分层窗口 + Direct2D + UpdateLayeredWindow。窗口不带穿透样式,
//! 所以能接收鼠标;按钮的动作复用服务端已有的请求处理。位置、大小与舞台的模型在
//! `lueying_render::toolbar`,角色行为在 `lueying_render::actor`,动画帧由动画包提供。
//! 动画包(`pack.json` 加帧图)放在服务端程序同目录的 `toolbar-pack` 里,没有就不画角色。
//! 角色分两层:按钮下面的先画,按钮上面的最后画。

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lueying_protocol::Request;
use lueying_render::actor::{Actor, Stage, View};
use lueying_render::animation::{Pack, frame_at};
use lueying_render::paint::{Canvas, Color};
use lueying_render::panel::Rect;
use lueying_render::toolbar::{Action, Button, Toolbar};
use lueying_server::skin_pack::{self, Layer, SkinPack};
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
/// 每一帧的间隔;工具栏上有角色时逐帧重画,没有角色时只在需要时重画。
const TICK: Duration = Duration::from_millis(33);
/// 圆角半径(倍率 1.0)。
const RADIUS: f32 = 9.0;
/// 字号(倍率 1.0)。
const FONT: f32 = 14.0;
/// 角色精灵的边长(倍率 1.0)。
const SPRITE: f32 = 36.0;

/// 窗口线程独占的状态:工具栏模型、两层舞台、动画包、窗口左上角的屏幕坐标、按钮交互。
struct Ctx {
    state: Arc<State>,
    hwnd: HWND,
    bar: Toolbar,
    screen: Rect,
    /// 按钮下面的角色。
    below: Stage,
    /// 按钮上面的角色。
    above: Stage,
    skin: Option<SkinPack>,
    started: Instant,
    /// 窗口左上角(屏幕坐标);鼠标坐标要加上它。
    origin: (f32, f32),
    /// 按下时落在哪个按钮上。
    pressed: Option<Button>,
    /// 按下后是否真的拖动过;拖动过就不算点击。
    moved: bool,
    dirty: bool,
    /// 上一次看到的按键计数;变了就说明用户又打了字。
    seen_keys: u64,
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
    let skin = load_skin(&mut surface);
    let area = bar.stage(screen);
    let below = cast(skin.as_ref(), Layer::Below, area, 0);
    let above = cast(skin.as_ref(), Layer::Above, area, 0);
    CTX.with(|cell| {
        *cell.borrow_mut() = Some(Ctx {
            state,
            hwnd,
            bar,
            screen,
            below,
            above,
            skin,
            started: Instant::now(),
            origin: (0.0, 0.0),
            pressed: None,
            moved: false,
            dirty: true,
            seen_keys: 0,
        });
    });
    loop {
        pump_messages();
        let animated = CTX.with(|cell| {
            cell.borrow().as_ref().is_some_and(|ctx| {
                !ctx.below.actors.is_empty() || !ctx.above.actors.is_empty()
            })
        });
        let dirty = CTX.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .map(|ctx| std::mem::take(&mut ctx.dirty))
                .unwrap_or(false)
        });
        if dirty || animated {
            CTX.with(|cell| {
                if let Some(ctx) = cell.borrow_mut().as_mut() {
                    redraw(ctx, &mut surface);
                }
            });
        }
        std::thread::sleep(TICK);
    }
}

/// 读动画包:`pack.json` 与帧图都在 `toolbar-pack` 里。缺了或坏了就不画角色,按钮照常。
fn load_skin(surface: &mut Surface) -> Option<SkinPack> {
    let dir = pack_dir()?;
    let text = std::fs::read_to_string(dir.join("pack.json")).ok()?;
    let skin = match skin_pack::parse(&text) {
        Ok(skin) => skin,
        Err(e) => {
            println!("工具栏动画包错误: {e}");
            return None;
        }
    };
    let frames = dir.join(&skin.frames);
    let last = skin.pack.max_frame()?;
    let mut sprites = Vec::with_capacity(last + 1);
    for number in 0..=last {
        match surface.load_sprite(&frames.join(format!("{number}.png"))) {
            Ok(bitmap) => sprites.push(bitmap),
            Err(e) => {
                println!("工具栏动画帧 {number} 读取失败: {e}");
                return None;
            }
        }
    }
    surface.set_sprites(sprites);
    Some(skin)
}

/// 把动画包里某一层的角色排上舞台:各组按数量建出来,同一层的角色均匀地站在舞台上。没有动画包时舞台是空的。
fn cast(skin: Option<&SkinPack>, layer: Layer, area: Rect, now_ms: u64) -> Stage {
    let mut stage = Stage::new(area, (SPRITE, SPRITE));
    let Some(skin) = skin else {
        return stage;
    };
    let specs: Vec<_> = skin.actors.iter().filter(|spec| spec.layer == layer).collect();
    let total: usize = specs.iter().map(|spec| spec.count).sum();
    let mut slot = 0;
    for spec in specs {
        for _ in 0..spec.count {
            slot += 1;
            let x = area.left + (area.right - area.left) * slot as f32 / (total + 1) as f32;
            let y = (area.top + area.bottom - SPRITE) / 2.0;
            let seed = (slot as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(now_ms);
            match Actor::new(seed, now_ms, (x - SPRITE / 2.0, y), spec.behavior.clone()) {
                Ok(actor) => stage.add(actor),
                Err(e) => println!("工具栏角色行为不合法: {e}"),
            }
        }
    }
    stage
}

/// 画一层角色:每个角色按自己的片段与时刻取帧,画在窗口的局部坐标里。
fn draw_actors(
    canvas: &mut Surface,
    stage: &Stage,
    views: &[View],
    pack: &Pack,
    now_ms: u64,
    origin: (f32, f32),
    size: f32,
) {
    for (actor, view) in stage.actors.iter().zip(views) {
        if let Some(index) = frame_at(actor.clip(), now_ms, pack) {
            let rect = Rect::from_xywh(view.x - origin.0, view.y - origin.1, size, size);
            canvas.draw_sprite(index, rect);
        }
    }
}

fn pack_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.join("toolbar-pack"))
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
    let cfg = {
        let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
        cfg.toolbar_pos = Some((x, y));
        cfg.clone()
    };
    if let Some(path) = &state.config_path {
        if let Err(e) = config::save(path, &cfg) {
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

/// 重画并上屏:先按时间推进角色,再按模型定窗口位置与大小;不可见时隐藏窗口。
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
    let stage_rect = ctx.bar.stage(ctx.screen);
    ctx.origin = (stage_rect.left, stage_rect.top);
    let (w, h) = (
        (stage_rect.right - stage_rect.left).max(1.0),
        (stage_rect.bottom - stage_rect.top).max(1.0),
    );
    let scale = ctx.bar.scale;
    let size = SPRITE * scale;
    let now_ms = ctx.started.elapsed().as_millis() as u64;
    for stage in [&mut ctx.below, &mut ctx.above] {
        stage.area = stage_rect;
        stage.sprite = (size, size);
    }
    // 用户打了字:舞台上的角色都收到一次按键,于是停下来看着输入。
    let keys = ctx.state.key_serial.load(Ordering::Relaxed);
    if keys != ctx.seen_keys {
        ctx.seen_keys = keys;
        for actor in ctx.below.actors.iter_mut().chain(ctx.above.actors.iter_mut()) {
            actor.key(now_ms);
        }
    }
    let below = ctx.below.tick(now_ms);
    let above = ctx.above.tick(now_ms);
    let palette = theme::palette(theme::colors_with(
        theme::Appearance::of_system(),
        place::ui_options(&ctx.state).2,
    ));
    let local = |r: Rect| Rect {
        left: r.left - stage_rect.left,
        top: r.top - stage_rect.top,
        right: r.right - stage_rect.left,
        bottom: r.bottom - stage_rect.top,
    };
    let bar_local = local(ctx.bar.rect());
    let labels = button_labels(&ctx.state);
    match surface.begin(w as u32, h as u32) {
        Ok(canvas) => {
            canvas.fill_rrect(bar_local, RADIUS * scale, Color::opaque(palette.bg));
            canvas.stroke_rrect(
                bar_local,
                RADIUS * scale,
                1.0,
                Color::opaque(palette.border),
            );
            let origin = (stage_rect.left, stage_rect.top);
            if let Some(skin) = &ctx.skin {
                draw_actors(canvas, &ctx.below, &below, &skin.pack, now_ms, origin, size);
            }
            for (button, rect) in ctx.bar.buttons() {
                let text = labels
                    .iter()
                    .find(|(b, _)| *b == button)
                    .map(|(_, text)| *text)
                    .unwrap_or("");
                canvas.text_centered(text, local(rect), FONT * scale, Color::opaque(palette.text));
            }
            if let Some(skin) = &ctx.skin {
                draw_actors(canvas, &ctx.above, &above, &skin.pack, now_ms, origin, size);
            }
        }
        Err(e) => println!("工具栏表面失败: {e}"),
    }
    if let Err(e) = surface.present(
        ctx.hwnd,
        stage_rect.left as i32,
        stage_rect.top as i32,
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
            // 在按钮以外、没有拖动地点了一下:看看点中了哪个角色,上层的优先。
            (None, _) => {
                if ctx.above.click(sx, sy).is_none() {
                    let _ = ctx.below.click(sx, sy);
                }
                None
            }
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
