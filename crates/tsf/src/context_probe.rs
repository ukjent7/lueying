//! 读光标左侧文本,组合开始时上报给 Server。
//!
//! 只用异步只读编辑会话:沉浸式应用的文本存区跨进程,同步会话会卡宿主。
//! 有组句时从组句起点往前读,没有组句时从当前选区起点往前读;这样无论
//! 探测会话在组合建立前还是建立后执行,读到的都是同一段左邻文本。

use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use pinyin_protocol::Request;
use windows::Win32::UI::TextServices::{
    ITfContext, ITfEditSession, ITfEditSession_Impl, TF_ANCHOR_START, TF_DEFAULT_SELECTION,
    TF_ES_READ, TF_SELECTION,
};
use windows::core::{Result, implement};

use crate::log;
use crate::service::{Shared, SharedClient};

/// 与 `pinyin_engine::CONTEXT_KEEP` 一致;DLL 不为了一个常量依赖整个引擎。
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
        let mut guard = self.client.borrow_mut();
        if let Some(client) = guard.as_mut() {
            let _ = client.round_trip(&Request::Context {
                session: self.session,
                text,
            });
        }
        Ok(())
    }
}

impl ContextProbe {
    fn read(&self, ec: u32) -> Option<String> {
        let range = match self.shared.composition.borrow().clone() {
            Some(composition) => unsafe { composition.GetRange().ok()? },
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
                unsafe { ManuallyDrop::take(&mut selection[0].range) }?
            }
        };
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
