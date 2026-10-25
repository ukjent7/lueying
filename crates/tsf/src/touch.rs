//! 触屏键盘首选布局。Win8 起触屏键盘在输入法激活时经 `ITfFunctionProvider`
//! 查询 `ITfFnGetPreferredTouchKeyboardLayout`;返回简体拼音优化布局，
//! 否则系统只显示该语言的默认布局。

use windows::Win32::Foundation::E_INVALIDARG;
use windows::Win32::UI::TextServices::{
    ITfFnGetPreferredTouchKeyboardLayout, ITfFnGetPreferredTouchKeyboardLayout_Impl, ITfFunction,
    ITfFunction_Impl, TKBLT_OPTIMIZED, TKBL_OPT_SIMPLIFIED_CHINESE_PINYIN, TKBLayoutType,
};
use windows::core::{BSTR, Error, Result, implement};

#[implement(ITfFunction, ITfFnGetPreferredTouchKeyboardLayout)]
pub(crate) struct TouchLayout;

impl ITfFunction_Impl for TouchLayout_Impl {
    fn GetDisplayName(&self) -> Result<BSTR> {
        Ok(BSTR::from(crate::NAME))
    }
}

impl ITfFnGetPreferredTouchKeyboardLayout_Impl for TouchLayout_Impl {
    fn GetLayout(&self, layout: *mut TKBLayoutType, id: *const u16) -> Result<()> {
        if layout.is_null() || id.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        unsafe {
            layout.write(TKBLT_OPTIMIZED);
            // 头文件把该参数写成 WORD*;写入简体拼音布局 ID。
            (id as *mut u16).write(TKBL_OPT_SIMPLIFIED_CHINESE_PINYIN as u16);
        }
        Ok(())
    }
}
