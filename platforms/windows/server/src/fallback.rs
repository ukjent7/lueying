//! 低层钩子回落通道:TSF 不覆盖的宿主也能打中文。
//!
//! 任务栏搜索框、Windows 输入体验这类 CoreTextEditContext 控件不建 TSF 文本区,
//! msctf 只把本 DLL 当显示属性提供者加载,从不激活输入法;线程级键馈也就没有
//! 挂钩的机会(实测 `WH_GETMESSAGE` 一次都不触发)。微信输入法覆盖这类宿主靠的
//! 是常驻进程里的全局键盘钩子,这里走同一条路。
//!
//! 只对已知不建 TSF 文本区的宿主生效(见 `FALLBACK_HOSTS`)。接管期间:
//! 键盘钩子吞掉按键,交给引擎按同一套映射组句,上屏时把字符注入前台窗口;
//! 候选窗复用界面线程那一套,起句前用 UIA 取宿主光标定位。候选窗会被
//! shell 浮层挡住点击,鼠标钩子把落在候选窗里的点击转发给它。
//!
//! 注入的按键带 `INJECT_MARK`,钩子只跳过自己的注入,不会回环。

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use lueying_protocol::{KeyMsg, Reply, Request};
use lueying_win_keys::{KeyTap, is_shift, vk_to_key};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetWindowThreadProcessId, KBDLLHOOKSTRUCT, LLKHF_UP,
    MSLLHOOKSTRUCT, SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
};

use windows::core::PWSTR;

use crate::imp::State;

/// 回落通道的合成会话号:高 32 位填一个真实进程号到不了的值(pid 不会超过 2^31)。
const SESSION: u64 = 0xF000_0000_0000_0001;

/// 自注入按键的标记:只认自己的注入,别的工具(宏、按键精灵、测试脚本)
/// 打进来的键照常处理,否则自动化与虚拟键盘都用不了输入法。
const INJECT_MARK: usize = 0x1E7E_C0DE;

const MOD_CTRL: u8 = 0x01;
const MOD_ALT: u8 = 0x02;
const MOD_SHIFT: u8 = 0x04;
const MOD_WIN: u8 = 0x08;

/// 钩子过程是自由函数,状态只能走全局;界面线程独占,不存在并发。
static STATE: OnceLock<Arc<State>> = OnceLock::new();

thread_local! {
    /// Shift 单击判定与「这个键被吞了、抬起也要吞」的记录。
    static TAP: KeyTap = KeyTap::new();
    static EATEN: std::cell::RefCell<std::collections::HashSet<u32>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    /// 上一次看到的前台窗口,用来判断「换窗口了」。
    static LAST_FG: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    /// 当前这轮组合是否还在进行;起句前才刷新候选窗锚点。
    static COMPOSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// 钩子事件里跟踪的修饰键;钩子线程没有输入队列,不能只信 `GetKeyState`。
    static MODS: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// 候选窗句柄缓存;窗口随服务进程存活,失效时再找。
    static CANDIDATE: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    /// 左键按下已被候选窗接管,抬起也要一并吞掉。
    static MOUSE_CAPTURED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// 上一次转发的鼠标位置,避免移动消息刷屏。
    static LAST_MOVE: std::cell::Cell<(i32, i32)> = const { std::cell::Cell::new((i32::MIN, i32::MIN)) };
}

/// 在一条专用线程上挂低层钩子并泵消息。
///
/// 低层钩子的回调只在「取消息」时派发,装在界面线程上会被它的等待拖住:
/// 界面线程空闲时是无限等待,而钩子回调不保证能唤醒它。专用线程只做
/// `GetMessage` 循环,钩子永远不会因为界面忙而丢键。
pub(crate) fn install(state: &Arc<State>) {
    if STATE.set(Arc::clone(state)).is_err() {
        return;
    }
    // 回落会话先建出来:引擎按会话组句,候选窗也只跟活跃会话出帧。
    let hello = Request::Hello {
        session: SESSION,
        host: "fallback".into(),
    };
    super::request::dispatch(state, hello);
    let spawned = std::thread::Builder::new()
        .name("fallback-hook".into())
        .spawn(run_hook_thread);
    if let Err(error) = spawned {
        eprintln!("低层钩子线程起不来: {error},系统应用暂不支持中文");
    }
}

fn run_hook_thread() {
    // 先碰一次消息队列:低层钩子的回调靠「向安装线程投递消息」派发,
    // 队列不存在时钩子可能挂上却永远收不到回调。
    let mut warm = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PeekMessageW(
            &mut warm,
            None,
            0,
            0,
            windows::Win32::UI::WindowsAndMessaging::PM_NOREMOVE,
        );
    }
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0) } {
        Ok(_) => println!("低层钩子回落通道已挂上"),
        Err(error) => {
            eprintln!("低层钩子挂钩失败: {error},系统应用暂不支持中文");
            return;
        }
    }
    // 候选窗在 shell 浮层的窗口带下面,真实点击会先被浮层吃掉;
    // 鼠标钩子把落在候选窗矩形里的点击转给候选窗,并吞掉原事件。
    match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0) } {
        Ok(_) => println!("低层鼠标回落已挂上"),
        Err(error) => eprintln!("低层鼠标钩子挂钩失败: {error},候选窗点选暂不可用"),
    }
    let mut msg = windows::Win32::UI::WindowsAndMessaging::MSG::default();
    loop {
        let result =
            unsafe { windows::Win32::UI::WindowsAndMessaging::GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
            windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // HC_ACTION(0) 才是真实按键。
    if code == 0 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        track_modifiers(info);
        if let Some(result) = handle(info) {
            return result;
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0
        && let Some(result) = handle_mouse(wparam, lparam)
    {
        return result;
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 候选窗在浮层下面,真实点击到不了它;把落在候选窗矩形里的鼠标事件
/// 经候选窗的注入口转过去,原事件吞掉。候选窗不用激活,搜索框保持焦点。
fn handle_mouse(wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let message = wparam.0 as u32;
    let rect = visible_candidate()?;
    let (x, y) = (info.pt.x as f32, info.pt.y as f32);
    let inside = x >= rect.0 as f32
        && x < (rect.0 + rect.2) as f32
        && y >= rect.1 as f32
        && y < (rect.1 + rect.3) as f32;
    use crate::ui::{forward_candidate_mouse, rune::FallbackMouse};
    match message {
        WM_LBUTTONDOWN if inside => {
            forward_candidate_mouse(FallbackMouse::Down(x, y));
            MOUSE_CAPTURED.with(|captured| captured.set(true));
            Some(LRESULT(1))
        }
        WM_LBUTTONUP if MOUSE_CAPTURED.with(std::cell::Cell::get) => {
            MOUSE_CAPTURED.with(|captured| captured.set(false));
            forward_candidate_mouse(FallbackMouse::Up);
            Some(LRESULT(1))
        }
        WM_RBUTTONDOWN if inside => {
            forward_candidate_mouse(FallbackMouse::Right(x, y));
            Some(LRESULT(1))
        }
        WM_MOUSEWHEEL if inside => {
            // WM_MOUSEWHEEL 的键状态在高 16 位;坐标按屏幕坐标给。
            let delta = ((info.mouseData >> 16) & 0xffff) as u16 as i16;
            forward_candidate_mouse(FallbackMouse::Wheel(delta));
            Some(LRESULT(1))
        }
        WM_MOUSEMOVE if inside || MOUSE_CAPTURED.with(std::cell::Cell::get) => {
            // 只转发悬停,不吞移动;同一点不重复发。
            if LAST_MOVE.with(|last| last.replace((info.pt.x, info.pt.y))) != (info.pt.x, info.pt.y)
            {
                forward_candidate_mouse(FallbackMouse::Move(x, y));
            }
            None
        }
        _ => None,
    }
}

/// 可见的候选窗矩形(屏幕物理像素);停靠/收起时返回 `None`。
/// 只有回落会话需要替候选窗接鼠标:TSF 宿主的候选窗本来就在最上面,
/// 别去重复转发、吞掉 Normal 应用的点击。
fn visible_candidate() -> Option<(i32, i32, i32, i32)> {
    let state = STATE.get()?;
    if state.active.load(std::sync::atomic::Ordering::Relaxed) != SESSION {
        return None;
    }
    state
        .candidate_rect
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .filter(|(_, _, w, h)| *w > 0 && *h > 0)
}

/// 实时按键状态;钩子线程不处理普通输入消息,`GetKeyState` 在这里不更新。
fn held(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

/// 从钩子事件里跟踪修饰键,补 `GetAsyncKeyState` 在注入键上的时差。
fn track_modifiers(info: &KBDLLHOOKSTRUCT) {
    let bit = match info.vkCode {
        0xA2 | 0xA3 => MOD_CTRL,
        0xA4 | 0xA5 => MOD_ALT,
        0xA0 | 0xA1 => MOD_SHIFT,
        0x5B | 0x5C => MOD_WIN,
        _ => return,
    };
    let down = info.flags.0 & LLKHF_UP.0 == 0;
    MODS.with(|mods| {
        let current = mods.get();
        mods.set(if down { current | bit } else { current & !bit });
    });
}

fn shift_held() -> bool {
    MODS.with(|mods| mods.get() & MOD_SHIFT != 0) || held(VK_SHIFT.0)
}

/// Ctrl/Alt/Win 任一按住:组合归系统,输入法不能吃。
fn command_held() -> bool {
    MODS.with(|mods| mods.get() & (MOD_CTRL | MOD_ALT | MOD_WIN) != 0)
        || held(VK_CONTROL.0)
        || held(VK_MENU.0)
        || held(VK_LWIN.0)
        || held(VK_RWIN.0)
}

/// 返回 `Some(1)` 表示吞掉这个按键,`None` 表示放行。
fn handle(info: &KBDLLHOOKSTRUCT) -> Option<LRESULT> {
    // 自己注入的字符不能再被自己接住;别人注入的键照常处理。
    if info.dwExtraInfo == INJECT_MARK {
        return None;
    }
    let state = STATE.get()?;
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    // 只接管已知不建 TSF 文本区的宿主。这些宿主即便激活了输入法也收不到键,
    // 「挂接」记录不可信,一律由回落通道处理;其余进程一律以 TSF 为准。
    if !is_fallback_host(pid) {
        return None;
    }
    // 宿主切到别的输入法后由 DLL 上报;没上报或已切走就把键全让给系统。
    if !state.fallback_active(pid) {
        COMPOSING.with(|composing| composing.set(false));
        return None;
    }
    // 前台换了窗口:把上一个窗口没落定的组合收起,别把旧拼音带进新窗口。
    LAST_FG.with(|last| {
        if last.get() != fg.0 as isize {
            last.set(fg.0 as isize);
            COMPOSING.with(|composing| composing.set(false));
            let _ = super::request::dispatch(
                state,
                Request::Key {
                    session: SESSION,
                    key: KeyMsg::Esc,
                },
            );
        }
    });
    let vk = info.vkCode;
    let up = info.flags.0 & LLKHF_UP.0 != 0;
    if up {
        return key_up(vk);
    }
    // Alt/Ctrl/Win 组合归系统(菜单、快捷键、输入法切换);钩子线程没有
    // 输入队列,`GetKeyState` 不可靠,统一查实时按键状态。
    if command_held() {
        return None;
    }
    TAP.with(|tap| tap.note_down(vk));
    if is_shift(vk) {
        return None;
    }
    let scan = info.scanCode;
    let key = vk_to_key(vk, scan, shift_held(), false)?;
    // 起句前取准光标:候选窗第一帧就落在当前插入点,不会先冒在旧位置。
    let starts = !COMPOSING.with(|composing| composing.get())
        && state.chinese.load(std::sync::atomic::Ordering::Relaxed)
        && matches!(key, KeyMsg::Letter(_) | KeyMsg::Digit(_) | KeyMsg::Punct(_));
    if starts {
        refresh_anchor(state);
    }
    let reply = dispatch(state, key)?;
    COMPOSING.with(|composing| composing.set(!reply.preedit.is_empty()));
    if reply.passthrough {
        return None;
    }
    inject(&reply.commits);
    EATEN.with(|eaten| eaten.borrow_mut().insert(vk));
    Some(LRESULT(1))
}

/// 已知把文本输入交给 CoreTextEditContext、从不激活输入法的宿主。
/// 系统搜索与输入体验实测:msctf 会加载本 DLL 并激活输入法,却不给文本区,
/// 按键也不走输入法所在线程的键馈。
const FALLBACK_HOSTS: &[&str] = &[
    "SearchHost.exe",
    "TextInputHost.exe",
    "StartMenuExperienceHost.exe",
];

fn is_fallback_host(pid: u32) -> bool {
    let Some(name) = process_name(pid) else {
        return false;
    };
    FALLBACK_HOSTS
        .iter()
        .any(|host| name.eq_ignore_ascii_case(host))
}

fn process_name(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    unsafe {
        let _ = CloseHandle(process);
    }
    if !ok {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    path.rsplit('\\').next().map(str::to_owned)
}

/// 起句前同步取准光标:钩子线程最多等 120ms。本机 UIA 查询通常 3ms,
/// 取不到就先用旧光标摆,不为此把键卡更久。
fn refresh_anchor(state: &State) {
    let Some(tx) = state.anchor_tx.as_ref() else {
        return;
    };
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    if pid == 0 || pid == std::process::id() {
        return;
    }
    let (reply, wait) = std::sync::mpsc::channel();
    let sent = tx.send(crate::uia::AnchorJob {
        pid,
        hwnd: fg.0 as u64,
        delay_ms: 0,
        reply,
    });
    if sent.is_err() {
        return;
    }
    // 一个会话里第一次跨进程查询要 60ms 左右,后面的都在 3ms 内;
    // 等不到就先用旧光标,不为此把键卡更久。
    let Ok(Some((left, top, right, bottom))) = wait.recv_timeout(Duration::from_millis(120)) else {
        return;
    };
    // 等结果这段时间用户已经切走:不能把候选窗摆到旧窗口上。
    if unsafe { GetForegroundWindow() } != fg {
        return;
    }
    let _ = super::request::dispatch(
        state,
        Request::Caret {
            session: SESSION,
            left,
            top,
            right,
            bottom,
        },
    );
}

/// 宿主窗口消息上报当前输入法状态;变了才打印,切走时收起回落组合。
pub(crate) fn report_active(state: &State, pid: u32, active: bool) {
    let changed = state.set_fallback_active(pid, active);
    if !changed {
        return;
    }
    println!("回落宿主 {pid} 输入法 active={active}");
    if !active {
        deactivate(state, pid);
    }
}

/// 宿主切到别的输入法:前台确是这个宿主时,收起回落组合与候选窗。
pub(crate) fn deactivate(state: &State, pid: u32) {
    if state.active.load(std::sync::atomic::Ordering::Relaxed) != SESSION {
        return;
    }
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return;
    }
    let mut current = 0u32;
    unsafe { GetWindowThreadProcessId(fg, Some(&mut current)) };
    if current != pid {
        return;
    }
    let _ = super::request::dispatch(
        state,
        Request::Key {
            session: SESSION,
            key: KeyMsg::Esc,
        },
    );
    state.activate(0);
}

/// 候选窗鼠标点选把上屏文本留在了 `ui_reply`;TSF 会话等下一次 Sync 取走,
/// 回落会话没有 Sync,点完就注入前台窗口。
pub(crate) fn flush(state: &State) {
    let Some(handle) = state.session(SESSION) else {
        return;
    };
    let commits = handle.with(state, |s| s.ui_reply.take().map(|reply| reply.commits));
    if let Some(commits) = commits {
        inject(&commits);
    }
}

fn key_up(vk: u32) -> Option<LRESULT> {
    let state = STATE.get()?;
    if is_shift(vk) && TAP.with(|tap| tap.take_shift_tap(vk)) {
        // Shift 单击:切中英,并把引擎给的上屏文本(通常为空)写出去。
        if let Some(reply) = dispatch(state, KeyMsg::Shift) {
            inject(&reply.commits);
        }
        return None;
    }
    let eaten = EATEN.with(|eaten| eaten.borrow_mut().remove(&vk));
    eaten.then_some(LRESULT(1))
}

fn dispatch(state: &State, key: KeyMsg) -> Option<Reply> {
    let reply = super::request::dispatch(
        state,
        Request::Key {
            session: SESSION,
            key,
        },
    );
    if !reply.passthrough
        && reply.chinese != state.chinese.load(std::sync::atomic::Ordering::Relaxed)
    {
        super::touch_hud(state);
    }
    super::touch_view(state);
    Some(reply)
}

/// 把上屏文本按 Unicode 注入前台窗口。宿主是普通完整性时走 SendInput,
/// 提权窗口会被 UIPI 拦下,这时退回 `PostMessage(WM_CHAR)`,字符仍能进去。
fn inject(commits: &[String]) {
    for text in commits {
        if text.is_empty() {
            continue;
        }
        let mut inputs = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            inputs.push(input(unit, KEYEVENTF_UNICODE));
            inputs.push(input(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
        if sent == inputs.len() as u32 {
            continue;
        }
        let fg = unsafe { GetForegroundWindow() };
        for unit in text.encode_utf16() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(fg),
                    windows::Win32::UI::WindowsAndMessaging::WM_CHAR,
                    WPARAM(unit as usize),
                    LPARAM(1),
                );
            }
        }
    }
    // 注入是异步的,给输入队列一点时间再继续组句。
    std::thread::sleep(Duration::from_millis(2));
}

fn input(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: Default::default(),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECT_MARK,
            },
        },
    }
}
