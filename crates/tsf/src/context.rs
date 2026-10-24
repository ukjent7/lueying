//! 上下文级开关:TSF 键盘类文本服务必须看 `GUID_COMPARTMENT_KEYBOARD_DISABLED`
//! (非零 = 所有键原样放行、不组句)。密码框走的就是它——微软文档明说密码框
//! 应当禁用文本服务(`IS_PASSWORD` 只是标注不提供保护),Chromium 系浏览器给
//! 密码框上下文设的也是它;`GUID_COMPARTMENT_EMPTYCONTEXT` 一并查(SampleIME
//! 同款)。查询失败按未禁,不因查询问题废掉正常输入。

use windows::Win32::UI::TextServices::{
    GUID_COMPARTMENT_EMPTYCONTEXT, GUID_COMPARTMENT_KEYBOARD_DISABLED, ITfCompartmentMgr,
    ITfContext,
};
use windows::core::{GUID, Interface};

/// 这个上下文禁了键盘(密码框、空上下文):所有键放行。
pub(crate) fn keyboard_disabled(context: &ITfContext) -> bool {
    let Ok(manager) = context.cast::<ITfCompartmentMgr>() else {
        return false;
    };
    flag(&manager, &GUID_COMPARTMENT_KEYBOARD_DISABLED)
        || flag(&manager, &GUID_COMPARTMENT_EMPTYCONTEXT)
}

/// compartment 里的 DWORD 非零才算禁;没设过/类型不对按 0。
fn flag(manager: &ITfCompartmentMgr, guid: &GUID) -> bool {
    unsafe { manager.GetCompartment(guid) }
        .and_then(|compartment| unsafe { compartment.GetValue() })
        .ok()
        .and_then(|variant| i32::try_from(&variant).ok())
        .is_some_and(|value| value != 0)
}
