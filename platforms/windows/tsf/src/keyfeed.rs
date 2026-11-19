//! 自驱键馈:替不驱动击键管理器的宿主把按键送进 TSF。
//!
//! TSF 感知的宿主(Chromium、Windows Terminal、新版记事本)自己 `Activate` 线程管理器后,
//! 系统就不再经 CUAS 代驱击键;而这些宿主从不调用 `ITfKeystrokeMgr::TestKeyDown/KeyDown`,
//! 前台键汇于是一个键也收不到。Firefox 显式调用、IMM32 应用由 CUAS 代驱,所以只有它们能打字。
//! 微信输入法的核心 DLL 同样导入 `SetWindowsHookExW` 自驱,这里照同一条路走:
//! 在宿主线程挂 `WH_GETMESSAGE`,取键时先替宿主走一遍击键管理器,吃掉的键把 `wParam`
//! 换成 `VK_PROCESSKEY`,宿主与 `TranslateMessage` 便不再重复处理。

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_PROCESSKEY;
use windows::Win32::UI::TextServices::ITfKeystrokeMgr;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HHOOK, MSG, PM_REMOVE, SetWindowsHookExW, UnhookWindowsHookEx, WH_GETMESSAGE,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::{DLL_MODULE, log};

struct Feed {
    keystroke: ITfKeystrokeMgr,
    hook: HHOOK,
    /// 钩子刚代驱过的键与结论;宿主随后对同一键再问一次时直接回放,不再往返 Server。
    last: Option<(u32, isize, bool)>,
}

thread_local! {
    static FEED: RefCell<Option<Feed>> = const { RefCell::new(None) };
}

/// 激活时挂上;同一线程重复激活只保留第一份。
pub(crate) fn install(keystroke: &ITfKeystrokeMgr) {
    FEED.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return;
        }
        let module = HINSTANCE(DLL_MODULE.load(Ordering::SeqCst));
        let hook = unsafe {
            SetWindowsHookExW(
                WH_GETMESSAGE,
                Some(get_message),
                Some(module),
                GetCurrentThreadId(),
            )
        };
        match hook {
            Ok(hook) => {
                *slot = Some(Feed {
                    keystroke: keystroke.clone(),
                    hook,
                    last: None,
                });
            }
            Err(error) => log(&format!("自驱键馈挂钩失败: {error}")),
        }
    });
}

pub(crate) fn remove() {
    FEED.with(|slot| {
        if let Some(feed) = slot.borrow_mut().take() {
            let _ = unsafe { UnhookWindowsHookEx(feed.hook) };
        }
    });
}

/// 钩子代驱过的同一键再次到达(宿主自己也驱动击键管理器)时回放结论。
pub(crate) fn replay(vk: u32, lparam: isize) -> Option<bool> {
    FEED.with(|slot| {
        // 代驱进行中持有写借用,取不到就说明这是钩子自己的第一次调用,走正常路径。
        slot.try_borrow()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(|feed| feed.last))
            .filter(|(key, lp, _)| *key == vk && *lp == lparam)
            .map(|(_, _, eaten)| eaten)
    })
}

unsafe extern "system" fn get_message(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == PM_REMOVE.0 {
        // 钩子里任何意外都不能穿过宿主的消息泵。
        let _ = catch_unwind(AssertUnwindSafe(|| drive(lparam.0 as *mut MSG)));
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn drive(msg: *mut MSG) {
    if msg.is_null() {
        return;
    }
    FEED.with(|slot| {
        // 代驱期间一直持有借用:重入的钩子与键汇回调取不到借用,天然被挡在外面。
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return;
        };
        let Some(feed) = slot.as_mut() else {
            return;
        };
        let msg = unsafe { &mut *msg };
        let down = matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN);
        if !down && !matches!(msg.message, WM_KEYUP | WM_SYSKEYUP) {
            return;
        }
        let (vk, lp) = (msg.wParam, msg.lParam);
        let keystroke = feed.keystroke.clone();
        let answer = unsafe {
            if down {
                keystroke.TestKeyDown(vk, lp)
            } else {
                keystroke.TestKeyUp(vk, lp)
            }
        };
        let eaten = answer.is_ok_and(|eaten| eaten.as_bool());
        // 抬键终结一次按键事件,清掉回放,免得下一次同键同扫描码命中旧结论。
        feed.last = down.then_some((vk.0 as u32, lp.0, eaten));
        if !eaten {
            return;
        }
        let _ = unsafe {
            if down {
                keystroke.KeyDown(vk, lp)
            } else {
                keystroke.KeyUp(vk, lp)
            }
        };
        // 键已被输入法消费:宿主按 IME 已处理对待,不再生成字符。
        msg.wParam = WPARAM(VK_PROCESSKEY.0 as usize);
    });
}
