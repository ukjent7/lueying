//! 组句内联下划线:TSF 显示属性协议(GUID → atom 写进范围的 GUID_PROP_ATTRIBUTE 属性)。

use core::cell::Cell;

use windows::Win32::Foundation::{E_INVALIDARG, S_FALSE};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, GUID_PROP_ATTRIBUTE, IEnumTfDisplayAttributeInfo,
    IEnumTfDisplayAttributeInfo_Impl, ITfCategoryMgr, ITfContext, ITfDisplayAttributeInfo,
    ITfDisplayAttributeInfo_Impl, ITfRange, TF_ATTR_INPUT, TF_CT_NONE, TF_DA_COLOR,
    TF_DISPLAYATTRIBUTE, TF_LS_DOT,
};
use windows::core::{BSTR, Error, GUID, Result, implement};

use crate::log;

/// 「输入中」显示属性 GUID,与注册的显示属性提供者类别配套。
pub(crate) const GUID_ATTR_INPUT: GUID = GUID::from_u128(0x4211bccd_dcf5_493d_baa2_2d98cba00749);

thread_local! {
    /// 属性 GUID 在类别管理器里的 atom,首次用时算出缓存。
    static ATOM: Cell<u32> = const { Cell::new(0) };
}

fn input_attribute() -> TF_DISPLAYATTRIBUTE {
    let follow_text = TF_DA_COLOR {
        r#type: TF_CT_NONE,
        ..Default::default()
    };
    TF_DISPLAYATTRIBUTE {
        crText: follow_text,
        crBk: follow_text,
        lsStyle: TF_LS_DOT,
        fBoldLine: false.into(),
        crLine: follow_text,
        bAttr: TF_ATTR_INPUT,
    }
}

/// 给组句范围打上下划线属性;失败只记日志。
pub(crate) fn mark(context: &ITfContext, ec: u32, range: &ITfRange) {
    if let Err(error) = mark_inner(context, ec, range) {
        log(&format!("组句下划线属性写入失败: {error}"));
    }
}

fn mark_inner(context: &ITfContext, ec: u32, range: &ITfRange) -> Result<()> {
    let variant = VARIANT::from(guid_atom()? as i32);
    let property = unsafe { context.GetProperty(&GUID_PROP_ATTRIBUTE)? };
    unsafe { property.SetValue(ec, range, &variant) }
}

fn guid_atom() -> Result<u32> {
    let cached = ATOM.with(Cell::get);
    if cached != 0 {
        return Ok(cached);
    }
    let manager: ITfCategoryMgr =
        unsafe { CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)? };
    let atom = unsafe { manager.RegisterGUID(&GUID_ATTR_INPUT)? };
    ATOM.with(|a| a.set(atom));
    Ok(atom)
}

#[implement(ITfDisplayAttributeInfo)]
struct AttributeInfo;

pub(crate) fn info() -> ITfDisplayAttributeInfo {
    AttributeInfo.into()
}

impl ITfDisplayAttributeInfo_Impl for AttributeInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_ATTR_INPUT)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from(crate::NAME))
    }

    fn GetAttributeInfo(&self, pda: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        if pda.is_null() {
            return Err(E_INVALIDARG.into());
        }
        unsafe { *pda = input_attribute() };
        Ok(())
    }

    fn SetAttributeInfo(&self, _pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        Ok(())
    }
}

#[implement(IEnumTfDisplayAttributeInfo)]
struct AttributeEnum {
    done: Cell<bool>,
}

pub(crate) fn enumerator() -> IEnumTfDisplayAttributeInfo {
    AttributeEnum {
        done: Cell::new(false),
    }
    .into()
}

impl IEnumTfDisplayAttributeInfo_Impl for AttributeEnum_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(AttributeEnum {
            done: Cell::new(self.done.get()),
        }
        .into())
    }

    fn Next(
        &self,
        ulcount: u32,
        rginfo: *mut Option<ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if ulcount >= 1 && rginfo.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let mut fetched = 0u32;
        if ulcount >= 1 && !self.done.get() {
            unsafe { *rginfo = Some(info()) };
            self.done.set(true);
            fetched = 1;
        }
        if !pcfetched.is_null() {
            unsafe { *pcfetched = fetched };
        }
        // 枚举器约定:取满 S_OK,不足 S_FALSE。
        if fetched == ulcount {
            Ok(())
        } else {
            Err(Error::from(S_FALSE))
        }
    }

    fn Reset(&self) -> Result<()> {
        self.done.set(false);
        Ok(())
    }

    fn Skip(&self, ulcount: u32) -> Result<()> {
        // 只有一个元素:跳过 0 个是成功且不改变位置;要跳的比剩下的多,
        // 走到末尾并回 S_FALSE。把 Skip(0) 当成取完会让后续 Next 全部空转。
        if ulcount == 0 {
            return Ok(());
        }
        let remaining = usize::from(!self.done.get());
        self.done.set(true);
        if ulcount as usize <= remaining {
            Ok(())
        } else {
            Err(Error::from(S_FALSE))
        }
    }
}
