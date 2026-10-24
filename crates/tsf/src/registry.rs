//! 自注册:COM InprocServer32(HKCR,regsvr32 要管理员)+ TSF profile 与能力类别。
//! 缺少沉浸式等能力声明时,Win10/11 的输入切换器会把这个输入法过滤掉(装上又消失)。

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
    GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_TIPCAP_COMLESS, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT, GUID_TFCAT_TIPCAP_SECUREMODE,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT, GUID_TFCAT_TIPCAP_UIELEMENTENABLED, ITfCategoryMgr,
    ITfInputProcessorProfiles,
};
use windows::core::{GUID, Result};
use windows_registry::CLASSES_ROOT;

use crate::{CLSID_IME, CLSID_STR, GUID_PROFILE, LANGID_ZH_CN, NAME};

const CATEGORIES: &[GUID] = &[
    GUID_TFCAT_TIP_KEYBOARD,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    GUID_TFCAT_TIPCAP_SECUREMODE,
    GUID_TFCAT_TIPCAP_COMLESS,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
];

fn clsid_key() -> String {
    format!("CLSID\\{CLSID_STR}")
}

pub(crate) fn register() -> Result<()> {
    let module = crate::module_path_file()
        .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
    let base = CLASSES_ROOT.create(clsid_key())?;
    base.set_string("", NAME)?;
    let inproc = base.create("InprocServer32")?;
    inproc.set_string("", &module.to_string_lossy())?;
    inproc.set_string("ThreadingModel", "Apartment")?;
    register_profile()
}

pub(crate) fn unregister() -> Result<()> {
    let _ = unregister_profile();
    let _ = CLASSES_ROOT.remove_tree(clsid_key());
    Ok(())
}

fn register_profile() -> Result<()> {
    com_scope(|| {
        let profiles: ITfInputProcessorProfiles = unsafe {
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?
        };
        // AddLanguageProfile 按 null 扫描读字符串,不补 0 会多读相邻内存。
        let description = wide_z(NAME);
        let icon = vec![0u16];
        unsafe {
            profiles.Register(&CLSID_IME)?;
            profiles.AddLanguageProfile(
                &CLSID_IME,
                LANGID_ZH_CN,
                &GUID_PROFILE,
                &description,
                &icon,
                0,
            )?;
        }
        let category: ITfCategoryMgr =
            unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };
        for catid in CATEGORIES {
            unsafe { category.RegisterCategory(&CLSID_IME, catid, &CLSID_IME)? };
        }
        Ok(())
    })
}

fn unregister_profile() -> Result<()> {
    com_scope(|| {
        if let Ok(category) = unsafe {
            CoCreateInstance::<_, ITfCategoryMgr>(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)
        } {
            for catid in CATEGORIES {
                let _ = unsafe { category.UnregisterCategory(&CLSID_IME, catid, &CLSID_IME) };
            }
        }
        if let Ok(profiles) = unsafe {
            CoCreateInstance::<_, ITfInputProcessorProfiles>(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            )
        } {
            unsafe {
                let _ = profiles.RemoveLanguageProfile(&CLSID_IME, LANGID_ZH_CN, &GUID_PROFILE);
                let _ = profiles.Unregister(&CLSID_IME);
            }
        }
        Ok(())
    })
}

/// regsvr32 一般已初始化过 COM;已是别的套间模式(RPC_E_CHANGED_MODE)就不配对反初始化。
fn com_scope<T>(body: impl FnOnce() -> Result<T>) -> Result<T> {
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = body();
    if initialized {
        unsafe { CoUninitialize() };
    }
    result
}

fn wide_z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
