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

/// 一次写回的内容。按键路径带 commit;停顿轮询只换 preedit。
pub(crate) struct Write {
    pub(crate) commit: Option<String>,
    pub(crate) preedit: String,
    /// 轮询写回发出时的状态;按键路径为 `None`。
    pub(crate) guard: Option<Guard>,
    /// 失焦落定:允许写回已经切走的旧输入框,只上屏、不再碰新输入框的组句。
    pub(crate) allow_stale: bool,
}

/// 停顿轮询写回的过期条件:发出后输入没变,文档里仍是那份内联文本。
pub(crate) struct Guard {
    pub(crate) revision: u64,
    pub(crate) base_preedit: String,
}

/// 一次文档更新:先上屏 commit,再放 preedit(空 = 收组句)。
#[implement(ITfEditSession)]
struct UpdateSession {
    context: ITfContext,
    session: u64,
    shared: Rc<Shared>,
    client: SharedClient,
    write: Write,
}

impl ITfEditSession_Impl for UpdateSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        // 同一线程里换输入框后,两个上下文的编辑会话执行顺序没有保证;
        // 旧输入框的写回不能落到新输入框的组句/光标状态上。
        let current = is_write_context(&self.shared, &self.context);
        if !current && !self.write.allow_stale {
            log(&format!(
                "编辑会话跳过:上下文已切换 commit={:?} preedit={:?}",
                self.write.commit, self.write.preedit
            ));
            return Ok(());
        }
        if !current {
            // 失焦落定:只把已经取到的上屏文本写回旧输入框。新输入框可能
            // 已经起了组句,不能碰共享组句状态,也不写内联拼音。
            if let Some(text) = &self.write.commit
                && let Err(error) = commit_text(&self.shared, &self.context, ec, text, true)
            {
                log(&format!("失焦落定失败: {error}"));
            }
            return Ok(());
        }
        if let Some(guard) = &self.write.guard
            && (self.shared.revision.get() != guard.revision
                || self.shared.preedit.borrow().as_str() != guard.base_preedit.as_str())
        {
            // 过期的是内联文本,不是已经取走的上屏文本。丢弃 commit 会把
            // 快速点选/别的窗口切模式产生的字吞掉;编辑会话按请求顺序执行,
            // 先落的旧 commit 不会覆盖随后按键自己的写回。
            if let Some(text) = &self.write.commit
                && let Err(error) = commit_text(&self.shared, &self.context, ec, text, false)
            {
                log(&format!("过期轮询上屏失败: {error}"));
            }
            return Ok(());
        }
        match catch_unwind(AssertUnwindSafe(|| {
            if let Some(text) = &self.write.commit {
                commit_text(&self.shared, &self.context, ec, text, false)?;
            }
            if self.write.preedit.is_empty() {
                end_composition(&self.shared, ec)
            } else {
                update_preedit(
                    &self.shared,
                    &self.client,
                    self.session,
                    &self.context,
                    ec,
                    &self.write.preedit,
                )
            }?;
            report_caret(&self.shared, &self.client, self.session, &self.context, ec);
            log(&format!(
                "编辑会话完成 commit={:?} preedit={:?}",
                self.write.commit, self.write.preedit
            ));
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
    write: Write,
) -> Result<()> {
    let edit: ITfEditSession = UpdateSession {
        context: context.clone(),
        session,
        shared,
        client,
        write,
    }
    .into();
    unsafe { context.RequestEditSession(client_id, &edit, TF_ES_READWRITE)? }.ok()
}

/// 有组句就把组句范围替换成文本再收组句,否则在选区插入。
/// `stale` 为失焦落定:只认属于这个上下文的组句,绝不清理新输入框的状态。
fn commit_text(
    shared: &Shared,
    context: &ITfContext,
    ec: u32,
    text: &str,
    stale: bool,
) -> Result<()> {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let composition = if stale {
        composition_for(shared, context)
    } else {
        live_composition(shared, context)
    };
    match composition {
        Some(composition) => {
            let range = unsafe { composition.GetRange()? };
            unsafe { range.SetText(ec, 0, &utf16)? };
            // 移动选区失败也要结束组句,否则旧输入框里会留下无法再管的拼音。
            if let Err(error) = move_selection_to_end(context, ec, &range) {
                log(&format!("上屏后移动光标失败: {error}"));
            }
            let ended = unsafe { composition.EndComposition(ec) };
            // 组句已经结束;部分上屏时随后的 update_preedit 要新起一个组句,
            // 不能复用这个已结束的对象。
            clear_composition(shared);
            ended?;
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

/// 只有最近一次按键所在的输入框能接收异步写回。
fn is_write_context(shared: &Shared, context: &ITfContext) -> bool {
    shared
        .last_context
        .borrow()
        .as_ref()
        .is_some_and(|current| current.as_raw() == context.as_raw())
}

/// 只取属于这个上下文的组句;上下文不一致时返回 `None` 且不改共享状态。
fn composition_for(shared: &Shared, context: &ITfContext) -> Option<ITfComposition> {
    let owned = shared
        .composition_context
        .borrow()
        .as_ref()
        .is_some_and(|owner| owner.as_raw() == context.as_raw());
    if !owned {
        return None;
    }
    shared.composition.borrow().clone()
}

/// 当前上下文自己的组句。上下文不一致时丢弃旧对象,避免异步会话在新输入框里
/// 复用旧输入框的组句(两个上下文的编辑会话队列互不影响,执行顺序无法保证)。
fn live_composition(shared: &Shared, context: &ITfContext) -> Option<ITfComposition> {
    let same = shared
        .composition_context
        .borrow()
        .as_ref()
        .is_some_and(|owner| owner.as_raw() == context.as_raw());
    if same {
        return shared.composition.borrow().clone();
    }
    if shared.composition.borrow().is_some() {
        clear_composition(shared);
    }
    None
}

fn clear_composition(shared: &Shared) {
    shared.composition.borrow_mut().take();
    shared.composition_context.borrow_mut().take();
    shared.preedit.borrow_mut().clear();
}

fn update_preedit(
    shared: &Rc<Shared>,
    client: &SharedClient,
    session: u64,
    context: &ITfContext,
    ec: u32,
    preedit: &str,
) -> Result<()> {
    let composition = match live_composition(shared, context) {
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
        context: context.clone(),
    }
    .into();
    let composition = unsafe { context_composition.StartComposition(ec, &range, &sink)? };
    *shared.composition.borrow_mut() = Some(composition.clone());
    *shared.composition_context.borrow_mut() = Some(context.clone());
    Ok(composition)
}

/// 清空组句文本再结束,避免残留拼音。
fn end_composition(shared: &Shared, ec: u32) -> Result<()> {
    // 先取出所有权再借 mut,否则 `if let` 会把 `Ref` 拖到整个分支末尾。
    let composition = shared.composition.borrow().clone();
    if let Some(composition) = composition {
        // 清文本失败也要结束组句,否则文档里会留下无法再管的拼音。
        if let Ok(range) = unsafe { composition.GetRange() } {
            let _ = unsafe { range.SetText(ec, 0, &[]) };
        }
        let _ = unsafe { composition.EndComposition(ec) };
    }
    clear_composition(shared);
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
    let composition = live_composition(shared, context)?;
    let range = unsafe { composition.GetRange().ok()? };
    // 量光标所在的串尾,而不是整段组句的左端:长拼音时候选窗要跟着光标走。
    unsafe { range.Collapse(ec, TF_ANCHOR_END).ok()? };
    let view: ITfContextView = unsafe { context.GetActiveView().ok()? };
    let mut rect = RECT::default();
    let mut clipped = windows::core::BOOL::default();
    if unsafe { view.GetTextExt(ec, &range, &mut rect, &mut clipped) }.is_ok()
        && rect.right > rect.left
        && rect.bottom > rect.top
    {
        return Some((rect.left, rect.top, rect.right, rect.bottom));
    }
    // 部分宿主(Java/Swing、自绘编辑器)不实现 GetTextExt。退回视图矩形,
    // 至少让候选窗出现在编辑区底部,而不是因为拿不到光标就完全消失。
    let view_rect = unsafe { view.GetScreenExt(ec) }.ok()?;
    (view_rect.right > view_rect.left && view_rect.bottom > view_rect.top).then_some((
        view_rect.left,
        view_rect.bottom,
        view_rect.right,
        view_rect.bottom,
    ))
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
    context: ITfContext,
}

impl ITfCompositionSink_Impl for CompositionSink_Impl {
    fn OnCompositionTerminated(
        &self,
        _ecwrite: u32,
        _composition: Ref<ITfComposition>,
    ) -> Result<()> {
        // 旧输入框的组句被终止时,当前输入框可能已经起了新组句;不能清掉新状态。
        let ours = self
            .shared
            .composition_context
            .borrow()
            .as_ref()
            .is_some_and(|owner| owner.as_raw() == self.context.as_raw());
        if !ours || !is_write_context(&self.shared, &self.context) {
            return Ok(());
        }
        log("组句被应用终止,清状态");
        clear_composition(&self.shared);
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
