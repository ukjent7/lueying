//! 编辑会话与组句写操作。TSF 不允许直接改文档,必须经 RequestEditSession;
//! 只用异步会话(不带 TF_ES_SYNC):沉浸式应用的文本存区跨进程,同步会话会崩宿主。
//! 会话末尾量组句/光标屏幕矩形报给 Server(候选窗定位)。

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use pinyin_protocol::Request;
use windows::Win32::Foundation::{E_FAIL, RECT};
use windows::Win32::UI::TextServices::{
    INSERT_TEXT_AT_SELECTION_FLAGS, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl,
    ITfContext, ITfContextComposition, ITfContextView, ITfEditSession, ITfEditSession_Impl,
    ITfInsertAtSelection, ITfRange, TF_AE_END, TF_ANCHOR_END, TF_ES_READWRITE, TF_IAS_QUERYONLY,
    TF_SELECTION, TF_SELECTIONSTYLE,
};
use windows::core::{Error, Interface, Ref, Result, implement};

use crate::attr;
use crate::client::Client;
use crate::log;
use crate::service::Shared;

type SharedClient = Rc<RefCell<Option<Client>>>;

/// 一次按键的文档更新:先上屏 commit,再放 preedit(空 = 收组句)。
#[implement(ITfEditSession)]
struct UpdateSession {
    context: ITfContext,
    session: u64,
    shared: Rc<Shared>,
    client: SharedClient,
    commit: Option<String>,
    preedit: String,
    /// 轮询写回要求发出时的状态计数仍未变;按键路径为 `None`。
    revision: Option<u64>,
    /// 轮询写回要求文档里的内联文本仍是发出时的那份;按键路径为 `None`。
    base_preedit: Option<String>,
}

impl ITfEditSession_Impl for UpdateSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        if let Some(revision) = self.revision
            && self.shared.revision.get() != revision
        {
            return Ok(());
        }
        if let Some(base) = &self.base_preedit
            && self.shared.preedit.borrow().as_str() != base
        {
            return Ok(());
        }
        match catch_unwind(AssertUnwindSafe(|| {
            if let Some(text) = &self.commit {
                commit_text(&self.shared, &self.context, ec, text)?;
            }
            if self.preedit.is_empty() {
                end_composition(&self.shared, ec)
            } else {
                update_preedit(
                    &self.shared,
                    &self.client,
                    self.session,
                    &self.context,
                    ec,
                    &self.preedit,
                )
            }?;
            report_caret(&self.shared, &self.client, self.session, &self.context, ec);
            Ok(())
        })) {
            Ok(result) => result,
            Err(_) => {
                log("编辑会话回调 panic(已兜住)");
                Err(Error::from(E_FAIL))
            }
        }
    }
}

/// 请求异步读写会话;写入结果在回调里记日志。
pub(crate) fn request_update(
    context: &ITfContext,
    client_id: u32,
    session: u64,
    shared: Rc<Shared>,
    client: SharedClient,
    commit: Option<String>,
    preedit: String,
    revision: Option<u64>,
    base_preedit: Option<String>,
) -> Result<()> {
    let edit: ITfEditSession = UpdateSession {
        context: context.clone(),
        session,
        shared,
        client,
        commit,
        preedit,
        revision,
        base_preedit,
    }
    .into();
    unsafe { context.RequestEditSession(client_id, &edit, TF_ES_READWRITE)? }.ok()
}

/// 有组句就把组句范围替换成文本再收组句,否则在选区插入。
fn commit_text(shared: &Shared, context: &ITfContext, ec: u32, text: &str) -> Result<()> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    match shared.composition.borrow().clone() {
        Some(composition) => {
            let range = unsafe { composition.GetRange()? };
            unsafe { range.SetText(ec, 0, &utf16)? };
            move_selection_to_end(context, ec, &range)?;
            unsafe { composition.EndComposition(ec)? };
            shared.preedit.borrow_mut().clear();
        }
        None => {
            let insert: ITfInsertAtSelection = context.cast()?;
            // 标志不能用 NOQUERY:它不回传 range,windows-rs 会把 NULL 当失败。
            let range = unsafe {
                insert.InsertTextAtSelection(ec, INSERT_TEXT_AT_SELECTION_FLAGS(0), &utf16)?
            };
            // 不移光标的话下一次插入又落在原处,字会从右往左堆。
            move_selection_to_end(context, ec, &range)?;
        }
    }
    Ok(())
}

fn update_preedit(
    shared: &Rc<Shared>,
    client: &SharedClient,
    session: u64,
    context: &ITfContext,
    ec: u32,
    preedit: &str,
) -> Result<()> {
    let composition = match shared.composition.borrow().clone() {
        Some(composition) => composition,
        None => start_composition(shared, client, session, context, ec)?,
    };
    let utf16: Vec<u16> = preedit.encode_utf16().collect();
    let range = unsafe { composition.GetRange()? };
    unsafe { range.SetText(ec, 0, &utf16)? };
    *shared.preedit.borrow_mut() = preedit.to_string();
    attr::mark(context, ec, &range);
    move_selection_to_end(context, ec, &range)
}

/// 在当前选区起一个空组句;组句 sink 交给框架持有。
fn start_composition(
    shared: &Rc<Shared>,
    client: &SharedClient,
    session: u64,
    context: &ITfContext,
    ec: u32,
) -> Result<ITfComposition> {
    let insert: ITfInsertAtSelection = context.cast()?;
    let range = unsafe { insert.InsertTextAtSelection(ec, TF_IAS_QUERYONLY, &[])? };
    let context_composition: ITfContextComposition = context.cast()?;
    let sink: ITfCompositionSink = CompositionSink {
        shared: shared.clone(),
        client: Rc::clone(client),
        session,
    }
    .into();
    let composition = unsafe { context_composition.StartComposition(ec, &range, &sink)? };
    *shared.composition.borrow_mut() = Some(composition.clone());
    Ok(composition)
}

/// 清空组句文本再结束,避免残留拼音。
fn end_composition(shared: &Shared, ec: u32) -> Result<()> {
    if let Some(composition) = shared.composition.borrow_mut().take() {
        let range = unsafe { composition.GetRange()? };
        unsafe { range.SetText(ec, 0, &[])? };
        unsafe { composition.EndComposition(ec)? };
    }
    shared.preedit.borrow_mut().clear();
    Ok(())
}

/// 量组句的屏幕矩形,报给 Server 摆候选窗。
fn report_caret(
    shared: &Shared,
    client: &SharedClient,
    session: u64,
    context: &ITfContext,
    ec: u32,
) {
    let Some((left, top, right, bottom)) = caret_rect(shared, context, ec) else {
        return;
    };
    let mut guard = client.borrow_mut();
    if let Some(c) = guard.as_mut() {
        let _ = c.round_trip(&Request::Caret {
            session,
            left,
            top,
            right,
            bottom,
        });
    }
}

fn caret_rect(shared: &Shared, context: &ITfContext, ec: u32) -> Option<(i32, i32, i32, i32)> {
    let composition = shared.composition.borrow().clone()?;
    let range = unsafe { composition.GetRange().ok()? };
    let view: ITfContextView = unsafe { context.GetActiveView().ok()? };
    let mut rect = RECT::default();
    let mut clipped = windows::core::BOOL::default();
    unsafe { view.GetTextExt(ec, &range, &mut rect, &mut clipped).ok()? };
    Some((rect.left, rect.top, rect.right, rect.bottom))
}

fn move_selection_to_end(context: &ITfContext, ec: u32, range: &ITfRange) -> Result<()> {
    let end = unsafe { range.Clone()? };
    unsafe { end.Collapse(ec, TF_ANCHOR_END)? };
    let selection = TF_SELECTION {
        range: ManuallyDrop::new(Some(end)),
        style: TF_SELECTIONSTYLE {
            ase: TF_AE_END,
            fInterimChar: false.into(),
        },
    };
    // SetSelection 不接管 range 的所有权,之后手动释放。
    let result = unsafe { context.SetSelection(ec, std::slice::from_ref(&selection)) };
    drop(ManuallyDrop::into_inner(selection.range));
    result
}

/// 应用强行终止组句(切窗口等):清本地组句并通知 Server。
#[implement(ITfCompositionSink)]
struct CompositionSink {
    shared: Rc<Shared>,
    client: SharedClient,
    session: u64,
}

impl ITfCompositionSink_Impl for CompositionSink_Impl {
    fn OnCompositionTerminated(
        &self,
        _ecwrite: u32,
        _composition: Ref<ITfComposition>,
    ) -> Result<()> {
        log("组句被应用终止,清状态");
        self.shared.composition.borrow_mut().take();
        self.shared.preedit.borrow_mut().clear();
        self.shared.touch();
        let mut guard = self.client.borrow_mut();
        if let Some(c) = guard.as_mut() {
            let _ = c.round_trip(&Request::CompositionTerminated {
                session: self.session,
            });
        }
        Ok(())
    }
}
