//! 回落宿主的输入法状态上报。
//!
//! 任务栏搜索这类宿主不建 TSF 文本区,按键由常驻服务的全局钩子接管。但宿主
//! 线程切到别的输入法后,钩子不能继续把键交给拼音引擎。本 DLL 已在宿主进程
//! 里,在宿主线程挂 `WH_GETMESSAGE`;每次消息到达时(节流)问一次 TSF 当前
//! 激活的键盘 profile,把「是不是掠影」投递给 Server 的候选窗。钩子据此让路。

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, GUID_TFCAT_TIP_KEYBOARD, ITfInputProcessorProfileMgr,
    ITfInputProcessorProfiles, TF_INPUTPROCESSORPROFILE, TF_PROFILETYPE_INPUTPROCESSOR,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, FindWindowW, PostMessageW, SetWindowsHookExW, WH_GETMESSAGE,
};
use windows::core::{Interface, w};

use crate::{CLSID_IME, DLL_MODULE, GUID_PROFILE, log};

/// 这些进程的键由常驻服务接管,需要知道线程当前选中的输入法。
const HOSTS: &[&str] = &[
    "SearchHost.exe",
    "TextInputHost.exe",
    "StartMenuExperienceHost.exe",
];

/// -1 = 还没查到;0 = 别的输入法;1 = 掠影。状态线程据此上报。
static ACTIVE: AtomicI32 = AtomicI32::new(-1);

static ONCE: std::sync::Once = std::sync::Once::new();

thread_local! {
    static MANAGER: RefCell<Option<ITfInputProcessorProfileMgr>> = const { RefCell::new(None) };
    static LAST_POLL: Cell<Option<Instant>> = const { Cell::new(None) };
    /// 查询本身可能触发 TSF 回调;用标志挡住重入。
    static POLLING: Cell<bool> = const { Cell::new(false) };
}

/// 在 TSF 首次取类对象时叫一次;只对回落宿主生效。
pub(crate) fn start_once() {
    ONCE.call_once(|| {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let host = exe
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| HOSTS.iter().any(|host| name.eq_ignore_ascii_case(host)));
        if !host {
            return;
        }
        log(&format!(
            "start pid={} exe={}",
            std::process::id(),
            exe.display()
        ));
        // 先查一次,状态线程不用等第一条消息。
        poll();
        log(&format!(
            "initial active={}",
            ACTIVE.load(Ordering::Relaxed)
        ));
        let module = HINSTANCE(DLL_MODULE.load(Ordering::SeqCst));
        let hook = unsafe {
            SetWindowsHookExW(
                WH_GETMESSAGE,
                Some(message_hook),
                Some(module),
                GetCurrentThreadId(),
            )
        };
        match hook {
            Ok(_) => log("输入法状态上报已挂上"),
            Err(error) => log(&format!("输入法状态挂钩失败: {error}")),
        }
        let _ = std::thread::Builder::new()
            .name("lueying-active".into())
            .spawn(status_loop);
    });
}

unsafe extern "system" fn message_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 {
        poll();
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 节流查询宿主线程当前激活的键盘 profile。
fn poll() {
    if POLLING.with(|polling| polling.replace(true)) {
        return;
    }
    let due = LAST_POLL.with(|last| {
        let now = Instant::now();
        let due = last
            .get()
            .is_none_or(|previous| now.duration_since(previous) >= Duration::from_millis(50));
        if due {
            last.set(Some(now));
        }
        due
    });
    if due {
        let active = active_now().unwrap_or(false);
        let value = i32::from(active);
        if ACTIVE.swap(value, Ordering::Relaxed) != value {
            log(&format!("输入法状态: active={active}"));
        }
    }
    POLLING.with(|polling| polling.set(false));
}

/// 宿主线程的激活键盘 profile 是否掠影。模式 compartment 是全系统共用
/// 的,别的输入法写它不关掠影的事,采纳前先问这一句。
pub(crate) fn active_now() -> Option<bool> {
    let manager = MANAGER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = create_manager();
        }
        slot.clone()
    })?;
    let mut profile = TF_INPUTPROCESSORPROFILE::default();
    if let Err(error) = unsafe { manager.GetActiveProfile(&GUID_TFCAT_TIP_KEYBOARD, &mut profile) }
    {
        log(&format!("GetActiveProfile: {error}"));
        return None;
    }
    Some(
        profile.dwProfileType == TF_PROFILETYPE_INPUTPROCESSOR
            && profile.clsid == CLSID_IME
            && profile.guidProfile == GUID_PROFILE,
    )
}

fn create_manager() -> Option<ITfInputProcessorProfileMgr> {
    unsafe {
        let profiles: ITfInputProcessorProfiles = CoCreateInstance(
            &CLSID_TF_InputProcessorProfiles,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
        .ok()?;
        profiles.cast().ok()
    }
}

/// 状态变化和每秒心跳时投递给服务端的候选窗;AppContainer 的管道被拦,
/// 窗口消息经 UIPI 放行,是唯一能跨这道边界的轻量通道。
fn status_loop() {
    let pid = std::process::id();
    let mut sent: Option<bool> = None;
    let mut sent_at = Instant::now() - Duration::from_secs(10);
    loop {
        let state = ACTIVE.load(Ordering::Relaxed);
        if state >= 0 {
            let active = state != 0;
            let due = sent != Some(active) || sent_at.elapsed() >= Duration::from_secs(1);
            if due
                // 窗口类是进程内的,跨进程 FindWindow 找不到;用唯一标题。
                && let Ok(window) = unsafe { FindWindowW(None, w!("lueying-candidate")) }
                && unsafe {
                    PostMessageW(
                        Some(window),
                        lueying_protocol::FALLBACK_ACTIVE_MESSAGE,
                        WPARAM(active as usize),
                        LPARAM(pid as isize),
                    )
                }
                .is_ok()
            {
                sent = Some(active);
                sent_at = Instant::now();
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
