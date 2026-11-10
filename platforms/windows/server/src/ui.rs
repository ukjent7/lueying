//! 界面:候选窗、模式徽标、工具栏与宿主选择面板,共用一条线程与一个消息循环。
//!
//! 循环只做三件事:等(窗口消息、唤醒事件或最近的动画截止时间)、排干消息、
//! 逐个窗口处理自己的脏标记。窗口过程仍在各自的模块里。某个窗口处理时崩了只停用
//! 它,其余窗口照常工作;输入法本身只依赖管道,与界面无关。

pub(crate) mod d2d;
mod hud;
pub(crate) mod picker;
mod place;
mod toolbar;

use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::HANDLE;
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
    candidate: Option<d2d::Candidate>,
    hud: Option<hud::Badge>,
    toolbar: Option<toolbar::Bar>,
    picker: Option<picker::Panel>,
}

impl Ui {
    fn new(state: &Arc<State>) -> Self {
        Self {
            candidate: d2d::init(state),
            hud: hud::init(state),
            toolbar: toolbar::init(state),
            picker: Some(picker::init(state, state.wake_ui)),
        }
    }

    fn tick(&mut self, state: &Arc<State>) {
        guard(&mut self.candidate, "D2D 候选窗", |window| {
            window.tick(state)
        });
        guard(&mut self.picker, "宿主选择面板", |window| {
            window.tick(state)
        });
        guard(&mut self.hud, "模式徽标", |window| window.tick(state));
        guard(&mut self.toolbar, "工具栏", |window| window.tick());
    }

    /// 下一次该醒来的时间:各窗口动画截止时间里最近的一个;都静止时为空。
    fn timeout(&self) -> Option<Duration> {
        let candidate = self.candidate.as_ref().and_then(d2d::Candidate::timeout);
        let hud = self.hud.as_ref().and_then(hud::Badge::timeout);
        [candidate, hud].into_iter().flatten().min()
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
