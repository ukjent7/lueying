//! 无状态类厂:造 TextService。

use core::ffi::c_void;

use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_FAIL};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::{BOOL, GUID, IUnknown, Interface, Ref, Result, implement};

use crate::service::TextService;

#[implement(IClassFactory)]
pub(crate) struct ClassFactory;

impl ClassFactory {
    pub(crate) fn new() -> Self {
        crate::lock_module();
        Self
    }
}

impl Drop for ClassFactory {
    fn drop(&mut self) {
        crate::unlock_module();
    }
}

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        if !punkouter.is_null() {
            return CLASS_E_NOAGGREGATION.ok();
        }
        if riid.is_null() || ppvobject.is_null() {
            return E_FAIL.ok();
        }
        let unknown: IUnknown = TextService::new().into();
        unsafe { unknown.query(riid, ppvobject).ok() }
    }

    fn LockServer(&self, flock: BOOL) -> Result<()> {
        if flock.as_bool() {
            crate::lock_module();
        } else {
            crate::unlock_module();
        }
        Ok(())
    }
}
