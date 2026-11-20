//! 界面:模式徽标、工具栏、宿主选择面板与回落消息桥,共用一条线程与一个消息循环。
//!
//! 循环只做两件事:等(窗口消息、唤醒事件或最近的动画截止时间)、逐个窗口
//! 处理自己的脏标记。窗口过程仍在各自的模块里。某个窗口处理时崩了只停用
//! 它,其余窗口照常工作;输入法本身只依赖管道,与界面无关。
//!
//! 候选窗在独立线程上(`ui::rune`),Rune 拥有自己的事件循环;这里只留
//! 一个隐形消息窗口,替 TSF 宿主接收「回落宿主激活/失活」的广播。

mod hud;
pub(crate) mod picker;
mod place;
pub(crate) mod rune;
mod toolbar;

use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Threading::INFINITE;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
    PeekMessageW, QS_ALLINPUT, TranslateMessage,
};

use crate::imp::{State, Wake};

/// 界面线程入口。窗口都建在这条线程上,循环不返回。
pub(super) fn run(state: Arc<State>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // 回落通道自带专用消息线程:低层钩子的回调靠取消息派发,
    // 不能挂在界面线程的等待上。
    crate::imp::fallback::install(&state);
    let mut ui = Ui::new(&state);
    ui.tick(&state);
    loop {
        wait(Some(state.wake_ui), ui.timeout());
        pump_messages();
        ui.tick(&state);
    }
}

/// 界面线程上的窗口。窗口建不出来时为 `None`,其余窗口照常。
struct Ui {
    hud: Option<hud::Badge>,
    toolbar: Option<toolbar::Bar>,
    picker: Option<picker::Panel>,
}

impl Ui {
    fn new(state: &Arc<State>) -> Self {
        bridge_window(state);
        Self {
            hud: hud::init(state),
            toolbar: toolbar::init(state),
            picker: Some(picker::init(state, state.wake_ui)),
        }
    }

    fn tick(&mut self, state: &Arc<State>) {
        guard(&mut self.picker, "宿主选择面板", |window| {
            window.tick(state)
        });
        guard(&mut self.hud, "模式徽标", |window| window.tick(state));
        guard(&mut self.toolbar, "工具栏", |window| window.tick());
    }

    /// 下一次该醒来的时间:各窗口动画截止时间里最近的一个;都静止时为空。
    fn timeout(&self) -> Option<Duration> {
        self.hud.as_ref().and_then(hud::Badge::timeout)
    }
}

/// 隐形消息窗:顶替候选窗接收 TSF 宿主的「回落宿主激活」广播。
/// 候选窗本体在 Rune 的事件循环里,收不了跨进程窗口消息。
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

/// 单个窗口的处理崩了就停用它,不带走其余窗口。
fn guard<T>(slot: &mut Option<T>, name: &str, run: impl FnOnce(&mut T)) {
    let Some(window) = slot.as_mut() else {
        return;
    };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(window))).is_err() {
        println!("{name}崩溃,已停用");
        *slot = None;
    }
}

/// 等一次:窗口消息、唤醒事件或截止时间,先到者返回。事件建不出来时
/// 退化成最长 15 毫秒的定时,线程不会变成满核忙等。
pub(crate) fn wait(wake: Option<Wake>, timeout: Option<Duration>) {
    let ms = timeout.map_or(INFINITE, |wait| {
        wait.as_millis().min(u128::from(u32::MAX)) as u32
    });
    match wake.map(Wake::raw) {
        Some(event) if !event.is_invalid() => unsafe {
            MsgWaitForMultipleObjectsEx(Some(&[event]), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        },
        _ => unsafe {
            let handles: Option<&[HANDLE]> = None;
            MsgWaitForMultipleObjectsEx(handles, ms.min(15), QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        },
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
