//! 候选窗背板材质:DWM 系统背板优先,accent policy 亚克力回退。
//!
//! Win11 22H2+ 用文档化的 `DWMWA_SYSTEMBACKDROP_TYPE`:毛玻璃由 DWM 合成器
//! 托管,在 DComp 视觉树的透明像素后面出底。旧系统不认这个属性,回退十年来
//! 被窗口美化工具广泛使用的未文档化 `SetWindowCompositionAttribute`——两条
//! 路径都不用改窗口样式。调用失败或系统关闭「透明效果」时,调用方回自绘实底。

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND, DWMWINDOWATTRIBUTE,
    DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::{BOOL, s, w};

/// 亚克力着色的不透明度;太薄读不清文字,太厚看不见毛玻璃。
const TINT_ALPHA: u32 = 0xb3;
/// WINDOWCOMPOSITIONATTRIBDATA 里 ACCENT_POLICY 的属性号。
const WCA_ACCENT_POLICY: u32 = 19;
/// ACCENT_ENABLE_ACRYLICBLURBEHIND:桌面在窗口背后被 DWM 模糊加噪。
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

/// 背板材质期望态;变化才落窗口属性。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Material {
    /// true = 毛玻璃背板;false = 实底主题。
    pub(super) on: bool,
    /// 回退亚克力的着色(0xRRGGBB),随主题底色走;系统背板的底色由 DWM 按深浅色定。
    pub(super) tint: u32,
    /// 跟随系统深色;决定系统背板的深浅底色。
    pub(super) dark: bool,
}

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attribute: u32,
    data: *const AccentPolicy,
    size_of_data: usize,
}

type SetWindowCompositionAttribute =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> BOOL;

/// 把材质落到窗口属性;返回毛玻璃是否真的启用,失败时调用方回实底。
pub(super) fn apply(hwnd: HWND, material: Material) -> bool {
    // 深浅色决定系统背板的底色,先落;旧系统静默忽略。
    set_attr(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, material.dark as i32);
    // Win11 22H2+ 走文档化系统背板;旧系统不认这个属性,回退 accent policy。
    let backdrop = if material.on {
        DWMSBT_TRANSIENTWINDOW
    } else {
        DWMSBT_NONE
    };
    let backdrop_ok = set_attr(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, backdrop.0);
    let applied = material.on && (backdrop_ok || accent_apply(hwnd, material));
    // Win11 的圆角由 DWM 裁;Win10 没有这个属性,静默忽略,方角亚克力可接受。
    let corner = if applied {
        DWMWCP_ROUND
    } else {
        DWMWCP_DONOTROUND
    };
    set_attr(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, corner.0);
    applied
}

/// 落单个 DWM 窗口属性;失败只回 false,不影响进程启动与出帧。
fn set_attr(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: i32) -> bool {
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            attribute,
            (&value as *const i32).cast(),
            std::mem::size_of::<i32>() as u32,
        )
        .is_ok()
    }
}

/// 未文档化 accent policy 亚克力:旧系统的回退路径。
fn accent_apply(hwnd: HWND, material: Material) -> bool {
    let policy = AccentPolicy {
        accent_state: if material.on {
            ACCENT_ENABLE_ACRYLICBLURBEHIND
        } else {
            0
        },
        accent_flags: 0,
        gradient_color: if material.on {
            // 亚克力着色是 0xAABBGGRR,与主题的 0xRRGGBB 字节序相反。
            TINT_ALPHA << 24
                | (material.tint & 0xff) << 16
                | (material.tint & 0xff00)
                | (material.tint >> 16)
        } else {
            0
        },
        animation_id: 0,
    };
    let mut data = WindowCompositionAttribData {
        attribute: WCA_ACCENT_POLICY,
        data: &policy,
        size_of_data: std::mem::size_of::<AccentPolicy>(),
    };
    material.on
        && composition_attribute().is_some_and(|set| unsafe { set(hwnd, &mut data).as_bool() })
}

/// 系统设置里的「透明效果」;关闭或查询失败都按关闭处理。
pub(super) fn transparency_enabled() -> bool {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("EnableTransparency"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
        .is_ok()
            && value != 0
    }
}

/// SetWindowCompositionAttribute 未公开;动态解析,导出缺失返回 None。
fn composition_attribute() -> Option<SetWindowCompositionAttribute> {
    let module = unsafe { GetModuleHandleW(w!("user32")) }.ok()?;
    // FARPROC 是 Option<函数指针> 的别名,导出缺失时为 None。
    let proc = unsafe { GetProcAddress(module, s!("SetWindowCompositionAttribute")) }?;
    // SAFETY:导出存在即签名与 ACCENT_POLICY 的公开用法一致。
    Some(unsafe {
        std::mem::transmute::<unsafe extern "system" fn() -> isize, SetWindowCompositionAttribute>(
            proc,
        )
    })
}
