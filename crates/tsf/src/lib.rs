#![cfg(windows)]
//! TSF 文本服务 DLL:加载进每个应用进程,拦截按键交给拼音引擎,
//! 经异步编辑会话写组句/上屏。激活流程、按键模式参考青简(qingjian)验证过的写法。

mod attr;
mod edit;
mod engine_host;
mod factory;
mod keys;
mod registry;
mod service;

use core::ffi::c_void;
use core::sync::atomic::{AtomicIsize, AtomicPtr, Ordering};

use windows::Win32::Foundation::{
    BOOL, CLASS_E_CLASSNOTAVAILABLE, E_FAIL, HINSTANCE, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::core::{GUID, HRESULT, Interface, PCWSTR};

/// 文本服务 CLSID(注册表 InprocServer32、TSF profile、DllGetClassObject 三处一致)。
pub(crate) const CLSID_IME: GUID = GUID::from_u128(0x8ffecd5b_dba4_480c_9749_fecdcdaa8fa17);

pub(crate) const CLSID_STR: &str = "{8FFECD5B-DBA4-480C-9749-FECDCAA8FA17}";

/// 语言 profile GUID。
pub(crate) const GUID_PROFILE: GUID = GUID::from_u128(0x76f58072_ab35_4aa7_ba2d_324f994587db);

/// zh-CN。
pub(crate) const LANGID_ZH_CN: u16 = 0x0804;

/// 系统里显示的输入法名。
pub(crate) const NAME: &str = "拼音";

/// 存活 COM 对象计数,DllCanUnloadNow 依据。
static DLL_REFERENCES: AtomicIsize = AtomicIsize::new(0);

/// DllMain 记下的模块句柄。
static DLL_MODULE: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

pub(crate) fn lock_module() {
    DLL_REFERENCES.fetch_add(1, Ordering::SeqCst);
}

pub(crate) fn unlock_module() {
    DLL_REFERENCES.fetch_sub(1, Ordering::SeqCst);
}

/// DLL 完整路径(注册 InprocServer32、找词典用)。
pub(crate) fn module_path_file() -> Option<std::path::PathBuf> {
    let module = HMODULE(DLL_MODULE.load(Ordering::SeqCst));
    let mut buf = [0u16; 512];
    let len = unsafe { GetModuleFileNameW(Some(module), &mut buf) } as usize;
    if len == 0 || len >= buf.len() {
        None
    } else {
        Some(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

/// 日志走 OutputDebugString(DebugView / VS 调试器可见)。
pub(crate) fn log(msg: &str) {
    let text: Vec<u16> = format!("[pinyin-ime] {msg}\n")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe { OutputDebugStringW(PCWSTR(text.as_ptr())) };
}

#[unsafe(no_mangle)]
extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_FAIL;
    }
    if unsafe { *rclsid } != CLSID_IME {
        return CLASS_E_CLASSNOTAVAILABLE;
    }
    let factory: windows::Win32::System::Com::IClassFactory = factory::ClassFactory.into();
    unsafe { factory.query(riid, ppv) }
}

#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    if DLL_REFERENCES.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match registry::register() {
        Ok(()) => S_OK,
        Err(error) => {
            log(&format!("DllRegisterServer 失败: {error}"));
            error.code()
        }
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    match registry::unregister() {
        Ok(()) => S_OK,
        Err(error) => {
            log(&format!("DllUnregisterServer 失败: {error}"));
            error.code()
        }
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        DLL_MODULE.store(hinst.0, Ordering::SeqCst);
    }
    true.into()
}
