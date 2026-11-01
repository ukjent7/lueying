//! 编辑会话与组句写操作。TSF 不允许直接改文档,必须经 RequestEditSession;
//! 只用异步会话(不带 TF_ES_SYNC):沉浸式应用的文本存区跨进程,同步会话会崩宿主。
//! 会话末尾量组句/光标屏幕矩形报给 Server(候选窗定位)。
//! 只经 IMM32 `CANDIDATEFORM` 报位置的宿主(Java AWT/Swing 一类 CUAS 应用),
//! 由本模块补发 `IMN_OPENCANDIDATE` 并采信那个落点。

use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use lueying_protocol::Request;
use windows::Win32::Foundation::{E_FAIL, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, MONITOR_DEFAULTTONULL, MonitorFromPoint};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, GetProcessDpiAwareness,
    LogicalToPhysicalPointForPerMonitorDPI, PROCESS_PER_MONITOR_DPI_AWARE,
};
use windows::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_EXCLUDE, CFS_POINT, IMN_CLOSECANDIDATE, IMN_OPENCANDIDATE,
    ImmGetCandidateWindow, ImmGetContext, ImmReleaseContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::TextServices::{
    INSERT_TEXT_AT_SELECTION_FLAGS, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl,
    ITfContext, ITfContextComposition, ITfContextView, ITfEditSession, ITfEditSession_Impl,
    ITfInsertAtSelection, ITfRange, TF_AE_END, TF_ANCHOR_END, TF_ANCHOR_START, TF_ES_READ,
    TF_ES_READWRITE, TF_IAS_QUERYONLY, TF_SELECTION, TF_SELECTIONSTYLE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GUITHREADINFO, GetAncestor, GetForegroundWindow, GetGUIThreadInfo, PostMessageW,
    WM_IME_NOTIFY,
};
use windows::core::{BOOL, Error, Interface, Ref, Result, implement};

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
            // 让宿主按上屏后的光标刷新 CANDIDATEFORM,下一次组合才有新位置。
            // 失焦落定时焦点可能已在别处,不能再往新窗口补发候选通知。
            if !stale {
                post_imm_candidate(true);
            }
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
    shared.caret_pending.set(false);
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
    shared.caret_pending.set(false);
    *shared.composition.borrow_mut() = Some(composition.clone());
    *shared.composition_context.borrow_mut() = Some(context.clone());
    // 已确认只经 IMM32 报位置的宿主,每次组合开始都要请它刷新候选窗落点。
    post_imm_candidate(false);
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
        let ended = unsafe { composition.EndComposition(ec) };
        // 收组句后补发一次 OPEN+CLOSE:Java AWT 收到 OPEN 才按当前光标
        // 重写 CANDIDATEFORM,CLOSE 让消息流与系统输入法成对。
        // 组句没真正结束时不通知宿主,免得它顺手关掉别的候选框。
        if ended.is_ok() {
            post_imm_candidate(true);
        }
    }
    clear_composition(shared);
    Ok(())
}

/// 宿主滚动/改布局后重新量一次组句矩形。只读会话,不改文档。
#[implement(ITfEditSession)]
struct CaretSession {
    context: ITfContext,
    session: u64,
    shared: Rc<Shared>,
    client: SharedClient,
}

impl ITfEditSession_Impl for CaretSession_Impl {
    fn DoEditSession(&self, ec: u32) -> Result<()> {
        self.shared.caret_pending.set(false);
        report_caret(&self.shared, &self.client, self.session, &self.context, ec);
        Ok(())
    }
}

/// 宿主布局变化时由 `ITfTextLayoutSink` 调用;候选窗跟着滚动后的光标走。
pub(crate) fn refresh_caret(
    context: &ITfContext,
    client_id: u32,
    session: u64,
    shared: Rc<Shared>,
    client: SharedClient,
) {
    // reflow 会连续触发多次;同一时间只排一个只读会话。
    if shared.caret_pending.replace(true) {
        return;
    }
    let edit: ITfEditSession = CaretSession {
        context: context.clone(),
        session,
        shared: shared.clone(),
        client,
    }
    .into();
    if let Err(error) = unsafe { context.RequestEditSession(client_id, &edit, TF_ES_READ) } {
        shared.caret_pending.set(false);
        log(&format!("布局变化重报光标失败: {error}"));
    }
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
    // `GetRange` 可能返回组合内部持有的活范围;先克隆再折叠,避免把组句
    // 本身收到串尾,后续内联文本写入就会丢掉整段。
    let caret = unsafe { range.Clone().ok()? };
    // 量光标所在的串尾,而不是整段组句的左端:长拼音时候选窗要跟着光标走。
    unsafe { caret.Collapse(ec, TF_ANCHOR_END).ok()? };
    // 组句起点只用于识别「宿主其实没在报位置」的退化指纹,不直接当锚点。
    let start = unsafe { range.Clone().ok()? };
    unsafe { start.Collapse(ec, TF_ANCHOR_START).ok()? };
    // 少数宿主不实现 GetActiveView;取不到视图仍可退回原生插入符。
    let view = unsafe { context.GetActiveView().ok() };
    let mut caret_rect = None;
    let mut start_rect = None;
    let mut comp_rect = None;
    let mut screen_ext = None;
    if let Some(view) = view.as_ref() {
        caret_rect = text_ext(view, ec, &caret);
        // 正常宿主的光标矩形已经可用,不必每键再量组句起点和整体;
        // 只有光标退化、落到屏幕外或本线程曾被判为 IMM 宿主时才继续。
        if !imm_candidate_host()
            && let Some(rect) = caret_rect.filter(valid_rect)
            && let Some(rect) = screen_rect(view, rect)
        {
            return Some(rect);
        }
        start_rect = text_ext(view, ec, &start);
        comp_rect = text_ext(view, ec, &range);
        screen_ext = unsafe { view.GetScreenExt() }.ok();
    }
    // Java AWT/Swing 这类走 CUAS 的宿主,对选区、组句起点、组句整体三次
    // 查询给出同一个退化角点(通常是工作区右下角)。单次退化可能只是还没
    // 排完版,三者全同才是「本 context 没有插入点可报」的证据。
    let host_default = is_host_default_position(caret_rect, start_rect, comp_rect);
    if host_default {
        note_host_default_position();
    } else if shows_laid_out_composition(caret_rect, comp_rect) {
        // 同一线程上的别的输入框确实在排版时解除判定,例如 Edge 的地址栏
        // 与网页输入框共用 UI 线程;Java 每帧都命中指纹,下次组合会重新置位。
        note_laid_out_composition();
    }
    if let Some(view) = view.as_ref() {
        if let Some(rect) = caret_rect.filter(valid_rect)
            && let Some(rect) = screen_rect(view, rect)
        {
            return Some(rect);
        }
        // 有些宿主对折叠到串尾的 range 回退化矩形,但对整个组合串能给出有效行框。
        // 取行框右端作为候选窗锚点,仍贴着光标所在行,比直接退回视图矩形准确。
        // 宿主明说没有插入点时不采信:那个「行框」只是工作区角点。
        if !host_default
            && let Some(line) = comp_rect.filter(valid_rect)
            && let Some(rect) = screen_rect(view, line)
        {
            return Some((rect.2, rect.1, rect.2, rect.3));
        }
    }
    // 走 IMM32→TSF 兼容层的宿主不实现可用的 GetTextExt,位置只经
    // `ImmSetCandidateWindow` 写进 CANDIDATEFORM。补发 IMN_OPENCANDIDATE
    // 后 AWT 才会刷新这个点;只在已证实的宿主上采信,并校验落点仍在
    // 宿主自己声明的显示区内,挡住上一次组合留下的陈旧坐标。
    if host_default
        && let Some(hwnd) = view.as_ref().and_then(|view| unsafe { view.GetWnd() }.ok())
        && let Some(rect) = imm_candidate_rect(hwnd, screen_ext)
    {
        return Some(rect);
    }
    // 部分宿主(Java/Swing、自绘编辑器)不实现 GetTextExt。系统仍可能知道
    // 前台线程的原生插入符位置;但命中过默认位置指纹的线程里,Java 的光标
    // 恒在客户区原点,采信它会把候选窗甩到窗口左上角。
    if !gui_caret_unreliable()
        && let Some(rect) = gui_caret()
    {
        return Some(rect);
    }
    // 最后退回视图矩形,至少让候选窗出现在编辑区底部,而不是完全消失。
    let view = view.as_ref()?;
    let mut view_rect = screen_ext?;
    if view_rect.right <= view_rect.left || view_rect.bottom <= view_rect.top {
        return None;
    }
    to_physical(view, &mut view_rect);
    on_screen(&view_rect).then_some((
        view_rect.left,
        view_rect.bottom,
        view_rect.right,
        view_rect.bottom,
    ))
}

thread_local! {
    /// 本线程已确认是「TSF 三次查询给同一个退化矩形、只经 IMM32 报位置」的宿主。
    /// 典型是 CUAS 下的 Java AWT/Swing。
    static IMM_CANDIDATE_HOST: Cell<bool> = const { Cell::new(false) };
    /// 本线程的原生插入符不可信(Java 恒在客户区原点)。只置不清。
    static GUI_CARET_UNRELIABLE: Cell<bool> = const { Cell::new(false) };
}

fn imm_candidate_host() -> bool {
    IMM_CANDIDATE_HOST.with(Cell::get)
}

fn gui_caret_unreliable() -> bool {
    GUI_CARET_UNRELIABLE.with(Cell::get)
}

/// 量一个 range 的屏幕矩形;宿主不实现 `GetTextExt` 时返回 `None`。
/// 退化矩形也原样返回,指纹判据需要看它是否三次完全一致。
fn text_ext(view: &ITfContextView, ec: u32, range: &ITfRange) -> Option<RECT> {
    let mut rect = RECT::default();
    let mut clipped = BOOL::default();
    unsafe { view.GetTextExt(ec, range, &mut rect, &mut clipped) }.ok()?;
    Some(rect)
}

/// 把宿主坐标矩形换成物理屏幕坐标;中心不在显示器上时返回 `None`。
fn screen_rect(view: &ITfContextView, mut rect: RECT) -> Option<(i32, i32, i32, i32)> {
    to_physical(view, &mut rect);
    on_screen(&rect).then_some((rect.left, rect.top, rect.right, rect.bottom))
}

fn valid_rect(rect: &RECT) -> bool {
    rect.right > rect.left && rect.bottom > rect.top
}

/// 选区、组句起点、组句整体三次查询是否给了同一个高度为零的矩形。
/// 缺任何一维都无法判断,按「不是」处理。
fn is_host_default_position(caret: Option<RECT>, start: Option<RECT>, comp: Option<RECT>) -> bool {
    let (Some(caret), Some(start), Some(comp)) = (caret, start, comp) else {
        return false;
    };
    caret.bottom <= caret.top && caret == start && caret == comp
}

/// 这一帧是不是宿主「确实在排版」的证据:选区有高度,组合矩形有宽有高,
/// 两者不同且水平、垂直都相交。用来解除 IMM32 宿主判定。
fn shows_laid_out_composition(caret: Option<RECT>, comp: Option<RECT>) -> bool {
    let (Some(caret), Some(comp)) = (caret, comp) else {
        return false;
    };
    caret.bottom > caret.top
        && comp.right > comp.left
        && comp.bottom > comp.top
        && caret != comp
        && caret.left >= comp.left
        && caret.left <= comp.right
        && caret.top < comp.bottom
        && caret.bottom > comp.top
}

/// 首次命中宿主默认位置:记住本线程并立即补发一次 OPEN,让当前这次组合
/// 也能拿到位置。系统光标同样标记为不可信。
fn note_host_default_position() {
    GUI_CARET_UNRELIABLE.with(|flag| flag.set(true));
    if IMM_CANDIDATE_HOST.with(|flag| !flag.replace(true)) {
        log("宿主未报插入点,补发 IMN_OPENCANDIDATE");
        post_imm_candidate(false);
    }
}

fn note_laid_out_composition() {
    if IMM_CANDIDATE_HOST.with(|flag| flag.replace(false)) {
        log("宿主恢复报位置,停止补发 IMN_OPENCANDIDATE");
        post_imm_close();
    }
}

/// 向本线程焦点窗口补发 IMN_OPENCANDIDATE;`then_close` 时紧跟一条关闭通知。
/// 用 Post 而非 Send:调用点在编辑会话内,同步送进宿主窗口过程有重入风险。
fn post_imm_candidate(then_close: bool) {
    if !imm_candidate_host() {
        return;
    }
    let hwnd = unsafe { GetFocus() };
    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_IME_NOTIFY,
            WPARAM(IMN_OPENCANDIDATE as usize),
            LPARAM(1),
        );
        if then_close {
            let _ = PostMessageW(
                Some(hwnd),
                WM_IME_NOTIFY,
                WPARAM(IMN_CLOSECANDIDATE as usize),
                LPARAM(1),
            );
        }
    }
}

fn post_imm_close() {
    let hwnd = unsafe { GetFocus() };
    if hwnd.is_invalid() {
        return;
    }
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_IME_NOTIFY,
            WPARAM(IMN_CLOSECANDIDATE as usize),
            LPARAM(1),
        );
    }
}

/// 宿主经 `ImmSetCandidateWindow` 声明的候选窗落点。
///
/// 只认 CANDIDATEFORM:COMPOSITIONFORM 答的是组合窗位置,不少宿主给的是
/// 输入框左上角。`ptCurrentPos` 是客户区坐标,`GetWnd` 在 CUAS 下就是 IMC
/// 所属窗口。落点校验失败就返回 `None`,退回视图矩形,不用陈旧坐标。
///
/// 已知缺口:纯 AWT 重量级控件按顶层窗口客户区原点换算,而 IMC 可能属于
/// 子窗口,结果会整体偏一个子窗口偏移;Swing 的焦点代理窗口在 (0,0),两者等价。
fn imm_candidate_rect(hwnd: HWND, screen_ext: Option<RECT>) -> Option<(i32, i32, i32, i32)> {
    if hwnd.is_invalid() {
        return None;
    }
    let himc = unsafe { ImmGetContext(hwnd) };
    if himc.is_invalid() {
        return None;
    }
    let mut form = CANDIDATEFORM::default();
    let got = unsafe { ImmGetCandidateWindow(himc, 0, &mut form) }.as_bool();
    let _ = unsafe { ImmReleaseContext(hwnd, himc) };
    if !got {
        return None;
    }
    // CFS_DEFAULT 表示宿主没表态,ptCurrentPos 无意义。
    if form.dwStyle != CFS_POINT && form.dwStyle != CFS_CANDIDATEPOS && form.dwStyle != CFS_EXCLUDE
    {
        return None;
    }
    let ext = screen_ext?;
    if ext.right <= ext.left || ext.bottom <= ext.top {
        return None;
    }
    // CANDIDATEFORM 给的是客户区坐标,先换成屏幕坐标再和显示区比较。
    let mut pt = form.ptCurrentPos;
    if !unsafe { ClientToScreen(hwnd, &mut pt) }.as_bool() {
        return None;
    }
    if pt.x < ext.left || pt.x > ext.right || pt.y < ext.top || pt.y > ext.bottom {
        return None;
    }
    // 落点是文本行下沿:角点换到物理坐标后,再按宿主 DPI 补一个默认行高。
    let mut rect = RECT {
        left: pt.x,
        top: pt.y,
        right: pt.x.saturating_add(1),
        bottom: pt.y,
    };
    to_physical_hwnd(hwnd, &mut rect);
    // 先按落点本身判断是否在显示器上。补行高后矩形中心可能越过屏幕上沿,
    // 但那是合法的顶部位置,交给 Server 摆到光标下方。
    if !on_screen(&rect) {
        return None;
    }
    // 宿主的 DPI:Per-Monitor 进程按窗口取,未感知的进程按系统取,
    // 补出来的行高才能和换算后的物理坐标同一尺度。
    let dpi = if per_monitor_aware() {
        unsafe { GetDpiForWindow(hwnd) }
    } else {
        unsafe { GetDpiForSystem() }
    };
    let dpi = if dpi == 0 { 96 } else { dpi };
    let height = (20 * dpi as i32 / 96).max(1);
    rect.top = rect.bottom - height;
    log(&format!(
        "采信 IMM32 CANDIDATEFORM ({}, {}),行高 {height}",
        pt.x, pt.y
    ));
    Some((rect.left, rect.top, rect.right, rect.bottom))
}

/// 宿主进程不是 Per-Monitor DPI 感知时,TSF 返回的是宿主坐标空间的逻辑像素;
/// 候选窗进程是 Per-Monitor V2,不换算会在高缩放和副屏上偏移。
/// 换算失败就保留原值,宁可在老宿主上偏一点,也不能丢掉候选窗。
fn to_physical(view: &ITfContextView, rect: &mut RECT) {
    let hwnd = unsafe { view.GetWnd() }.unwrap_or_else(|_| unsafe { GetForegroundWindow() });
    if !hwnd.is_invalid() {
        to_physical_hwnd(hwnd, rect);
    }
}

fn per_monitor_aware() -> bool {
    unsafe { GetProcessDpiAwareness(None) }
        .is_ok_and(|awareness| awareness == PROCESS_PER_MONITOR_DPI_AWARE)
}

fn to_physical_hwnd(hwnd: HWND, rect: &mut RECT) {
    if per_monitor_aware() {
        return;
    }
    let mut top_left = POINT {
        x: rect.left,
        y: rect.top,
    };
    let mut bottom_right = POINT {
        x: rect.right,
        y: rect.bottom,
    };
    let converted = unsafe {
        LogicalToPhysicalPointForPerMonitorDPI(Some(hwnd), &mut top_left).as_bool()
            && LogicalToPhysicalPointForPerMonitorDPI(Some(hwnd), &mut bottom_right).as_bool()
    };
    if converted {
        rect.left = top_left.x;
        rect.top = top_left.y;
        rect.right = bottom_right.x;
        rect.bottom = bottom_right.y;
    }
}

/// 矩形中心是否落在真实显示器上。部分自绘宿主(全屏游戏、假文本 store)会
/// 成功返回屏幕外的常量矩形;这种坐标不能拿去摆候选窗,否则窗口会钉在
/// 屏幕角落。多屏副屏的负坐标仍会命中对应显示器。
fn on_screen(rect: &RECT) -> bool {
    let center = POINT {
        x: rect.left + (rect.right - rect.left) / 2,
        y: rect.top + (rect.bottom - rect.top) / 2,
    };
    !unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// 当前宿主线程的原生插入符矩形(屏幕坐标)。TSF 的 GetTextExt 在 Java/Swing
/// 和自绘编辑器里常失败,但同一线程的 Win32 插入符仍可能有效。
/// 取当前线程而非前台线程:焦点切换后前台线程可能已是别的应用。
///
/// 同一线程上也可能有别的窗口(工具栏、任务栏残留)持有光标;顶层的根窗口
/// 和焦点窗口不一致时不能采信,否则候选窗会跑到另一个窗口上。
fn gui_caret() -> Option<(i32, i32, i32, i32)> {
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let tid = unsafe { GetCurrentThreadId() };
    unsafe { GetGUIThreadInfo(tid, &mut info) }.ok()?;
    if info.hwndCaret.is_invalid() {
        return None;
    }
    let focus = unsafe { GetFocus() };
    if !focus.is_invalid() {
        let caret_root = unsafe { GetAncestor(info.hwndCaret, GA_ROOT) };
        let focus_root = unsafe { GetAncestor(focus, GA_ROOT) };
        if !caret_root.is_invalid() && !focus_root.is_invalid() && caret_root != focus_root {
            return None;
        }
    }
    let rect = info.rcCaret;
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return None;
    }
    // GUITHREADINFO.rcCaret 是 hwndCaret 的客户区坐标,不是屏幕坐标;
    // 不换算会把候选窗放到窗口左上角。
    let mut top_left = POINT {
        x: rect.left,
        y: rect.top,
    };
    let mut bottom_right = POINT {
        x: rect.right,
        y: rect.bottom,
    };
    unsafe {
        if !ClientToScreen(info.hwndCaret, &mut top_left).as_bool()
            || !ClientToScreen(info.hwndCaret, &mut bottom_right).as_bool()
        {
            return None;
        }
    }
    let mut screen = RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    };
    to_physical_hwnd(info.hwndCaret, &mut screen);
    on_screen(&screen).then_some((screen.left, screen.top, screen.right, screen.bottom))
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
