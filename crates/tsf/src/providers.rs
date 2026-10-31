//! 显示属性、函数提供与布局通知三个小提供者。
//!
//! `service` 的 `#[path]` 子模块(src 保持扁平)。

use windows::Win32::Foundation::{E_INVALIDARG, E_NOINTERFACE};
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, ITfContextView, ITfDisplayAttributeInfo,
    ITfDisplayAttributeProvider_Impl, ITfFnGetPreferredTouchKeyboardLayout,
    ITfFunctionProvider_Impl, ITfTextLayoutSink_Impl, TF_LC_CHANGE, TfLayoutCode,
};
use windows::core::{BSTR, Error};

use super::*;

impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(attr::enumerator())
    }

    fn GetDisplayAttributeInfo(&self, guid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        if guid.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        if unsafe { *guid } == attr::GUID_ATTR_INPUT {
            Ok(attr::info())
        } else {
            Err(Error::from(E_INVALIDARG))
        }
    }
}

impl ITfFunctionProvider_Impl for TextService_Impl {
    fn GetType(&self) -> Result<GUID> {
        Ok(crate::CLSID_IME)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from(crate::NAME))
    }

    fn GetFunction(&self, _group: *const GUID, riid: *const GUID) -> Result<IUnknown> {
        // 触屏键盘按 IID 查询首选布局;设置/候选搜索等函数按需再加。
        if !riid.is_null() && unsafe { *riid } == ITfFnGetPreferredTouchKeyboardLayout::IID {
            let layout: ITfFnGetPreferredTouchKeyboardLayout = crate::touch::TouchLayout.into();
            return Ok(layout.into());
        }
        Err(Error::from(E_NOINTERFACE))
    }
}

impl ITfTextLayoutSink_Impl for TextService_Impl {
    fn OnLayoutChange(
        &self,
        pic: Ref<ITfContext>,
        code: TfLayoutCode,
        _view: Ref<ITfContextView>,
    ) -> Result<()> {
        // 宿主滚动/缩放后重新量光标;只在当前输入框仍在组句时做,避免空转。
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let Ok(context) = pic.ok() else {
                return;
            };
            let ours = self
                .shared
                .composition_context
                .borrow()
                .as_ref()
                .is_some_and(|owner| owner.as_raw() == context.as_raw());
            // 宿主自绘候选时服务端窗口已收起,不必再为坐标做编辑会话。
            if ours && code == TF_LC_CHANGE && !self.shared.ui_host_draws.get() {
                edit::refresh_caret(
                    context,
                    self.client_id.get(),
                    self.shared.session,
                    self.shared.clone(),
                    self.client.clone(),
                );
            }
        }));
        Ok(())
    }
}
