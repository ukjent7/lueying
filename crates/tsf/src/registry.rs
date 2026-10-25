//! 自注册:COM InprocServer32(HKCR,regsvr32 要管理员)+ TSF profile 与能力类别。
//! 缺少沉浸式等能力声明时,Win10/11 的输入切换器会把这个输入法过滤掉(装上又消失)。

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
    GUID_TFCAT_TIP_KEYBOARD, GUID_TFCAT_TIPCAP_COMLESS, GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT, GUID_TFCAT_TIPCAP_SECUREMODE,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT, GUID_TFCAT_TIPCAP_UIELEMENTENABLED, ITfCategoryMgr,
    ITfInputProcessorProfileMgr, ITfInputProcessorProfiles,
};
use windows::core::{GUID, Interface, Result};
use windows_registry::CLASSES_ROOT;

use crate::{CLSID_IME, CLSID_STR, GUID_PROFILE, LANGID_ZH_CN, NAME};

const CATEGORIES: &[GUID] = &[
    GUID_TFCAT_TIP_KEYBOARD,
    GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
    GUID_TFCAT_TIPCAP_COMLESS,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
];

/// 旧版本声明过、现已撤销的能力。安全桌面里本 DLL 会自拉常驻服务,
/// 从用户可写目录执行进程会变成 SYSTEM 提权面;未做安全模式隔离前不声明。
const REVOKED_CATEGORIES: &[GUID] = &[GUID_TFCAT_TIPCAP_SECUREMODE];

fn clsid_key() -> String {
    format!("CLSID\\{CLSID_STR}")
}

/// windows-registry(0.100 线)与 windows 0.62 分属 windows-rs 两条版本线,错误类型不同,
/// 按 HRESULT 显式搬运,两个 crate 才能各自用最新稳定版。
pub(crate) fn register() -> Result<()> {
    let Some(module) = crate::module_path_file() else {
        return Err(windows::core::Error::from(
            windows::Win32::Foundation::E_FAIL,
        ));
    };
    write_registry(&module)
        .map_err(|e| windows::core::Error::from_hresult(windows::core::HRESULT(e.code().0)))?;
    register_profile()
}

fn write_registry(module: &std::path::Path) -> windows_registry::Result<()> {
    let base = CLASSES_ROOT.create(clsid_key())?;
    base.set_string("", NAME)?;
    let inproc = base.create("InprocServer32")?;
    inproc.set_string("", module.to_string_lossy())?;
    inproc.set_string("ThreadingModel", "Apartment")?;
    Ok(())
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
        unsafe { profiles.Register(&CLSID_IME)? };
        // Win8+ 的 ProfileMgr 能声明“默认启用”,否则用户还要去语言设置里手动添加。
        // 老接口作为兜底(Win7 语义;本机只支持 Win10+,但接口缺失时不至于装不上)。
        let modern: std::result::Result<ITfInputProcessorProfileMgr, _> = unsafe {
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
        };
        let registered = match modern {
            Ok(manager) => {
                let name: Vec<u16> = NAME.encode_utf16().collect();
                let icon_file: Vec<u16> = crate::module_path_file()
                    .map(|path| path.to_string_lossy().encode_utf16().collect())
                    .unwrap_or_default();
                unsafe {
                    manager.RegisterProfile(
                        &CLSID_IME,
                        LANGID_ZH_CN,
                        &GUID_PROFILE,
                        &name,
                        &icon_file,
                        0,
                        HKL::default(),
                        0,
                        true,
                        0,
                    )
                }
            }
            Err(_) => Err(windows::core::Error::from(
                windows::Win32::Foundation::E_NOTIMPL,
            )),
        };
        if registered.is_err() {
            unsafe {
                profiles.AddLanguageProfile(
                    &CLSID_IME,
                    LANGID_ZH_CN,
                    &GUID_PROFILE,
                    &description,
                    &icon,
                    0,
                )?;
            }
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
            for catid in CATEGORIES.iter().chain(REVOKED_CATEGORIES) {
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
                if let Ok(manager) = profiles.cast::<ITfInputProcessorProfileMgr>() {
                    let _ = manager.UnregisterProfile(
                        &CLSID_IME,
                        LANGID_ZH_CN,
                        &GUID_PROFILE,
                        0,
                    );
                }
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
