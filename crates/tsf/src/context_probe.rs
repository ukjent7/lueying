//! 读光标左侧文本,组合开始时上报给 Server。
//!
//! 只用异步只读编辑会话:沉浸式应用的文本存区跨进程,同步会话会卡宿主。
//! 有组句时从组句起点往前读,没有组句时从当前选区起点往前读;这样无论
//! 探测会话在组合建立前还是建立后执行,读到的都是同一段左邻文本。

use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use lueying_protocol::Request;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::{VT_UNKNOWN, VariantClear};
use windows::Win32::UI::TextServices::{
    GUID_PROP_INPUTSCOPE, IS_ALPHANUMERIC_PIN, IS_NUMERIC_PASSWORD, IS_NUMERIC_PIN, IS_PASSWORD,
    IS_PRIVATE, ITfContext, ITfEditSession, ITfEditSession_Impl, ITfInputScope, ITfRange,
    InputScope, TF_ANCHOR_START, TF_DEFAULT_SELECTION, TF_ES_READ, TF_SELECTION,
};
use windows::core::{Interface, Result, implement};

use crate::log;
use crate::service::{Shared, SharedClient};

/// 与 `lueying_engine::CONTEXT_KEEP` 一致;DLL 不为了一个常量依赖整个引擎。
const KEEP: usize = 128;

/// 在当前文档上排一次左邻文本读取;已有读取在途时跳过。
pub(crate) fn request(
    context: &ITfContext,
    client_id: u32,
    session: u64,
    shared: Rc<Shared>,
    client: SharedClient,
) {
    if shared.composing() || shared.probing.replace(true) {
        return;
    }
    let epoch = shared.context_epoch.get();
    let edit: ITfEditSession = ContextProbe {
        context: context.clone(),
        shared: shared.clone(),
        client,
        session,
        epoch,
    }
    .into();
    let requested =
        unsafe { context.RequestEditSession(client_id, &edit, TF_ES_READ) }.and_then(|hr| hr.ok());
    if requested.is_err() {
        shared.probing.set(false);
    }
}

#[implement(ITfEditSession)]
struct ContextProbe {
    context: ITfContext,
    shared: Rc<Shared>,
    client: SharedClient,
    session: u64,
    epoch: u64,
}

impl ITfEditSession_Impl for ContextProbe_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        self.shared.probing.set(false);
        if self.shared.context_epoch.get() != self.epoch {
            return Ok(());
        }
        let private = match catch_unwind(AssertUnwindSafe(|| self.private(ec))) {
            Ok(private) => private,
            Err(_) => {
                log("输入范围读取 panic(已兜住)");
                false
            }
        };
        // 私密字段不读左邻文本,也不让它进入重排上下文;只上报私密标记。
        let text = if private {
            String::new()
        } else {
            let text = match catch_unwind(AssertUnwindSafe(|| self.read(ec))) {
                Ok(text) => text,
                Err(_) => {
                    log("左邻文本读取 panic(已兜住)");
                    return Ok(());
                }
            };
            let Some(text) = text else {
                return Ok(());
            };
            text
        };
        let mut guard = self.client.borrow_mut();
        if let Some(client) = guard.as_mut() {
            let _ = client.round_trip(&Request::Context {
                session: self.session,
                text,
                private,
            });
        }
        Ok(())
    }
}

impl ContextProbe {
    /// 当前选区/组句所在的 range;输入范围属性挂在这段文本上。
    fn range(&self, ec: u32) -> Option<ITfRange> {
        match self.shared.composition.borrow().clone() {
            Some(composition) => unsafe { composition.GetRange().ok() },
            None => {
                let mut selection = [TF_SELECTION::default()];
                let mut fetched = 0u32;
                unsafe {
                    self.context
                        .GetSelection(ec, TF_DEFAULT_SELECTION, &mut selection, &mut fetched)
                        .ok()?;
                }
                if fetched == 0 {
                    return None;
                }
                unsafe { ManuallyDrop::take(&mut selection[0].range) }
            }
        }
    }

    fn private(&self, ec: u32) -> bool {
        let Some(range) = self.range(ec) else {
            return false;
        };
        input_scope_private(&self.context, ec, &range).unwrap_or(false)
    }

    fn read(&self, ec: u32) -> Option<String> {
        let range = self.range(ec)?;
        let range = unsafe { range.Clone().ok()? };
        unsafe { range.Collapse(ec, TF_ANCHOR_START).ok()? };
        let mut shifted = 0i32;
        unsafe {
            range
                .ShiftStart(ec, -(KEEP as i32), &mut shifted, std::ptr::null())
                .ok()?;
        }
        let mut buf = [0u16; KEEP * 2];
        let mut count = 0u32;
        unsafe { range.GetText(ec, 0, &mut buf, &mut count).ok()? };
        let count = (count as usize).min(buf.len());
        let text: String = String::from_utf16_lossy(&buf[..count])
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        Some(text)
    }
}

/// `GUID_PROP_INPUTSCOPE` 是否包含 `IS_PRIVATE`。读不到按非私密处理,
/// 密码框仍由 `GUID_COMPARTMENT_KEYBOARD_DISABLED` 兜底。
fn input_scope_private(context: &ITfContext, ec: u32, range: &ITfRange) -> Option<bool> {
    const SECRET_SCOPES: [InputScope; 5] = [
        IS_PASSWORD,
        IS_PRIVATE,
        IS_NUMERIC_PASSWORD,
        IS_NUMERIC_PIN,
        IS_ALPHANUMERIC_PIN,
    ];
    // 输入范围是应用设置的属性;Chromium 用 IS_PRIVATE 标记密码框与无痕窗口。
    let property = unsafe { context.GetAppProperty(&GUID_PROP_INPUTSCOPE) }.ok()?;
    let mut value = unsafe { property.GetValue(ec, range) }.ok()?;
    let result = (|| {
        let variant = unsafe { &value.Anonymous.Anonymous };
        if variant.vt != VT_UNKNOWN {
            return Some(false);
        }
        let unknown = unsafe { &*variant.Anonymous.punkVal };
        let scope: ITfInputScope = unknown.as_ref()?.cast().ok()?;
        let mut scopes: *mut InputScope = std::ptr::null_mut();
        let mut count = 0u32;
        unsafe { scope.GetInputScopes(&mut scopes, &mut count) }.ok()?;
        let private = if scopes.is_null() || count == 0 {
            false
        } else {
            let list = unsafe { std::slice::from_raw_parts(scopes, count as usize) };
            list.iter().any(|scope| SECRET_SCOPES.contains(scope))
        };
        if !scopes.is_null() {
            unsafe { CoTaskMemFree(Some(scopes as *const core::ffi::c_void)) };
        }
        Some(private)
    })();
    // VARIANT 里的 IUnknown 由 VariantClear 释放;上面 cast 已 AddRef。
    unsafe {
        let _ = VariantClear(&mut value);
    }
    result
}
