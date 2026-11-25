//! 界面:隐形消息桥。候选窗、模式徽标、工具栏与宿主选择面板都在
//! `ui::rune` 的 Rune App 里(主线程);这条线程只剩一个隐形消息窗口,
//! 替 TSF 宿主接收「回落宿主激活/失活」的广播。
//!
//! 输入法本身只依赖管道,与界面无关;消息桥建不出来时回落广播收不到,
//! 兜底会话的点击与失活检测退化为轮询,输入照常。

pub(crate) mod badge;
pub(crate) mod lens;
pub(crate) mod material;
pub(crate) mod picker;
pub(crate) mod place;
pub(crate) mod rune;
pub(crate) mod toolbar;

use std::sync::Arc;

use windows::Win32::Foundation::{HANDLE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Threading::INFINITE;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, QS_ALLINPUT, TranslateMessage,
};

use crate::imp::State;

/// 候选窗要重画了:Rune overlay 自己的 poll 读版本号决定是否重跑 view。
pub(crate) fn kick_candidate() {
    rune::kick();
}

/// 叫醒候选窗、模式徽标与工具栏:三者是同一 App 里相互独立的 overlay,
/// 各自的 poll 只有各自的句柄 update 才够得着。
pub(crate) fn kick_hud() {
    kick_candidate();
    badge::kick();
    toolbar::kick();
}

/// 配置变了叫醒工具栏(倍率、标点、简繁的状态字都在它的 poll 里对读)。
pub(crate) fn kick_toolbar() {
    toolbar::kick();
}

pub(crate) fn run_candidate(state: Arc<State>) {
    rune::run(state);
}

/// 界面线程入口。窗口都建在这条线程上,循环不返回。
pub(super) fn run(state: Arc<State>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // 回落通道自带专用消息线程:低层钩子的回调靠取消息派发,
    // 不能挂在界面线程的等待上。
    crate::imp::fallback::install(&state);
    bridge_window(&state);
    loop {
        // 只等消息:广播来了 wnd_proc 自己处理,这条线程没有别的活。
        unsafe {
            let handles: Option<&[HANDLE]> = None;
            MsgWaitForMultipleObjectsEx(handles, INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
        pump_messages();
    }
}

/// 隐形消息窗:顶替候选窗接收 TSF 宿主的「回落宿主激活」广播。
/// 候选窗本体在 Rune App 的主线程上。
fn bridge_window(state: &Arc<State>) {
    use std::sync::OnceLock;

    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, MSGFLT_ALLOW, RegisterClassW,
        SW_HIDE, ShowWindow, WNDCLASSW, WS_POPUP,
    };
    use windows::core::{PCWSTR, w};

    static BRIDGE_STATE: OnceLock<Arc<State>> = OnceLock::new();
    unsafe extern "system" fn wnd_proc(
        hwnd: windows::Win32::Foundation::HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if msg == lueying_protocol::FALLBACK_ACTIVE_MESSAGE
            && let Some(state) = BRIDGE_STATE.get()
        {
            crate::imp::fallback::report_active(state, lparam.0 as u32, wparam.0 != 0);
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    // 广播按标题找窗,标题沿用旧候选窗的「lueying-candidate」。
    if BRIDGE_STATE.set(Arc::clone(state)).is_err() {
        return;
    }
    unsafe {
        let class = w!("lueying-candidate-bridge");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return;
        }
        let Ok(hwnd) = CreateWindowExW(
            Default::default(),
            class,
            w!("lueying-candidate"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(HINSTANCE(GetModuleHandleW(None).unwrap_or_default().0)),
            None,
        ) else {
            return;
        };
        let _ = ShowWindow(hwnd, SW_HIDE);
        let _ = ChangeWindowMessageFilterEx(
            hwnd,
            lueying_protocol::FALLBACK_ACTIVE_MESSAGE,
            MSGFLT_ALLOW,
            None,
        );
        let _: Option<PCWSTR> = None;
    }
}

/// 排干本线程的窗口消息。
pub(crate) fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
