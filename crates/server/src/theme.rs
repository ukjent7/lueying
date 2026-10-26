//! 系统主题查询:高对比度判定与系统颜色换算。
//! 候选窗和设置窗共用,避免两处各写一份判定后走样。

use std::ffi::c_void;

use windows::Win32::Graphics::Gdi::{GetSysColor, SYS_COLOR_INDEX};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

/// 系统是否处于高对比度主题。查询失败按普通主题处理。
pub fn high_contrast() -> bool {
    let mut info = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            info.cbSize,
            Some((&mut info as *mut HIGHCONTRASTW).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok()
        && info.dwFlags.contains(HCF_HIGHCONTRASTON)
}

/// COLORREF(0x00BBGGRR)→ GPUI 的 0xRRGGBB。
pub fn system_color(index: SYS_COLOR_INDEX) -> u32 {
    let color = unsafe { GetSysColor(index) };
    ((color & 0xff) << 16) | (color & 0xff00) | ((color >> 16) & 0xff)
}
