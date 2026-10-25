//! 文本服务主体:激活/停用、按键接收、焦点通知、显示属性提供。
//! 键在 TestKeyDown 阶段经管道送 Server 预演并缓存应答,OnKeyDown 只应用文档更新;
//! Server 不在时全部放行(字母直通,防死键盘)。

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;
use std::time::Instant;

use pinyin_protocol::{Reply, Request};
use windows::Win32::Foundation::{E_INVALIDARG, FALSE, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, ITfComposition, ITfContext, ITfDisplayAttributeInfo,
    ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl, ITfKeyEventSink,
    ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfLangBarItemButton, ITfLangBarItemSink, ITfSource,
    ITfTextInputProcessor, ITfTextInputProcessor_Impl, ITfThreadFocusSink,
    ITfThreadFocusSink_Impl, ITfThreadMgr, TF_LBI_ICON, TF_LBI_TEXT, TF_LBI_TOOLTIP,
};
use windows::core::{BOOL, Error, GUID, IUnknownImpl, Interface, Ref, Result, implement};

use crate::attr;
use crate::client::{Client, RETRY_INTERVAL};
use crate::edit;
use crate::keys::{self, KeyTap};
use crate::log;

/// 跨按键存活的组句状态(STA 单线程,Rc 传递)。
pub(crate) struct Shared {
    pub composition: RefCell<Option<ITfComposition>>,
    /// 组句所属的上下文;异步编辑会话跨输入框执行时不能复用旧上下文的组句。
    pub composition_context: RefCell<Option<ITfContext>>,
    /// 文档里当前的内联文本,轮询时用来判断异步结果是否已经写过。
    pub preedit: RefCell<String>,
    /// 最近一次按键的上下文,停顿重排写回文档时申请编辑会话用。
    pub last_context: RefCell<Option<ITfContext>>,
    /// 按键/焦点/组句变化计数;轮询结果必须带着发出时的计数才能落地。
    pub revision: Cell<u64>,
    /// 左邻文本读取正在进行;避免组合建立前每个键都排一次读。
    pub probing: Cell<bool>,
    /// 焦点代次;异步读到的旧窗口文本不能覆盖新窗口的上下文。
    pub context_epoch: Cell<u64>,
    /// 会话号:宿主线程 id。
    pub session: u64,
    /// 连接退避,语言栏点击与按键路径共用。
    pub last_connect: Cell<Option<Instant>>,
    /// 当前 TSF client id(编辑会话要用)。
    pub tid: Cell<u32>,
    /// 中文模式;语言栏指示器与 Caps 恢复用它。
    pub chinese: Cell<bool>,
    /// 线程管理器:写系统「转换模式」「输入法开/关」compartment 用。
    pub thread_mgr: RefCell<Option<ITfThreadMgr>>,
    /// 正在写 compartment;期间收到的 OnChange 是这次写入的回声,不再上报。
    pub mode_writing: Cell<bool>,
    /// 语言栏更新回调,由按钮在 AddItem 后登记。
    pub langbar_sink: RefCell<Option<ITfLangBarItemSink>>,
}

impl Shared {
    pub(crate) fn composing(&self) -> bool {
        self.composition.borrow().is_some()
    }

    pub(crate) fn touch(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    pub(crate) fn bump_context_epoch(&self) {
        self.context_epoch
            .set(self.context_epoch.get().wrapping_add(1));
        self.probing.set(false);
    }

    /// 记下中/英,写系统模式 compartment,再让语言栏重绘(只在变化时通知)。
    pub(crate) fn set_chinese(&self, chinese: bool) {
        if self.chinese.replace(chinese) == chinese {
            return;
        }
        if let Some(thread_mgr) = self.thread_mgr.borrow().as_ref() {
            let previous = self.mode_writing.replace(true);
            crate::mode::write_indicator(thread_mgr, self.tid.get(), chinese);
            self.mode_writing.set(previous);
        }
        self.refresh_langbar();
    }

    /// Caps Lock 等外部状态变了、但中/英没变时也要刷新图标。
    pub(crate) fn refresh_langbar(&self) {
        if let Some(sink) = self.langbar_sink.borrow().as_ref() {
            let flags = TF_LBI_ICON | TF_LBI_TEXT | TF_LBI_TOOLTIP;
            unsafe {
                let _ = sink.OnUpdate(flags);
            }
        }
    }
}

pub(crate) type SharedClient = Rc<RefCell<Option<Client>>>;

/// TestKeyDown 缓存、待 KeyDown 写文档的应答。
struct Pending {
    context: ITfContext,
    reply: Reply,
    /// 产生这条应答的虚拟键;OnKeyDown 必须对得上才应用。
    vk: u32,
}

#[implement(
    ITfTextInputProcessor,
    ITfKeyEventSink,
    ITfThreadFocusSink,
    ITfDisplayAttributeProvider
)]
pub(crate) struct TextService {
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    client_id: Cell<u32>,
    client: SharedClient,
    shared: Rc<Shared>,
    tap: KeyTap,
    pending: RefCell<Option<Pending>>,
    source: RefCell<Option<ITfSource>>,
    focus_cookie: Cell<u32>,
    /// 中/英语言栏按钮;激活时登记,停用时移除。
    langbar: RefCell<Option<ITfLangBarItemButton>>,
    /// 系统模式 compartment 的监听;停用时撤销。
    mode_advice: RefCell<crate::mode::Advice>,
    /// Caps Lock 亮起前的模式,灭灯后恢复。
    caps_restore: Cell<Option<bool>>,
    /// 组句期间的停顿轮询定时器;停用时自动停表。
    poll: RefCell<Option<crate::poll::PollTimer>>,
}

fn host_app_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

impl TextService {
    pub(crate) fn new() -> Self {
        let session = u64::from(unsafe { GetCurrentThreadId() });
        let service = Self {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            client: Rc::new(RefCell::new(None)),
            shared: Rc::new(Shared {
                composition: RefCell::new(None),
                composition_context: RefCell::new(None),
                preedit: RefCell::new(String::new()),
                last_context: RefCell::new(None),
                revision: Cell::new(0),
                probing: Cell::new(false),
                context_epoch: Cell::new(0),
                session,
                last_connect: Cell::new(None),
                tid: Cell::new(0),
                chinese: Cell::new(true),
                thread_mgr: RefCell::new(None),
                mode_writing: Cell::new(false),
                langbar_sink: RefCell::new(None),
            }),
            tap: KeyTap::new(),
            pending: RefCell::new(None),
            source: RefCell::new(None),
            focus_cookie: Cell::new(0),
            langbar: RefCell::new(None),
            mode_advice: RefCell::new(Vec::new()),
            caps_restore: Cell::new(None),
            poll: RefCell::new(None),
        };
        // COM 对象活着时不能让 DllCanUnloadNow 放行卸载。
        crate::lock_module();
        service
    }

    fn mapped_key(&self, vk: u32) -> Option<pinyin_protocol::KeyMsg> {
        if keys::command_mods() || keys::caps_on() {
            return None;
        }
        keys::vk_to_key(vk, keys::shift_down())
    }

    /// 发请求(懒连接 + 退避);连上时先 Hello。
    fn request(&self, req: Request) -> Option<Reply> {
        request_with(&self.client, &self.shared, req)
    }

    /// 把应答写进文档(异步编辑会话,会话里量光标报给 Server 摆候选窗)。
    fn apply(&self, context: &ITfContext, reply: &Reply) {
        // 新按键让任何在途的停顿写回失效。
        self.shared.touch();
        self.shared.set_chinese(reply.chinese);
        let commit: String = reply.commits.concat();
        if let Err(error) = edit::request_update(
            context,
            self.client_id.get(),
            self.shared.session,
            self.shared.clone(),
            self.client.clone(),
            edit::Write {
                commit: (!commit.is_empty()).then_some(commit),
                preedit: reply.preedit.clone(),
                guard: None,
                allow_stale: false,
            },
        ) {
            log(&format!("编辑会话请求失败: {error}"));
        }
    }

    /// 失焦落定:应答可能在新输入框的按键之后才执行,只上屏、不改新组句。
    fn apply_stale(&self, context: &ITfContext, reply: &Reply) {
        self.shared.set_chinese(reply.chinese);
        let commit = reply.commits.concat();
        if commit.is_empty() {
            return;
        }
        if let Err(error) = edit::request_update(
            context,
            self.client_id.get(),
            self.shared.session,
            self.shared.clone(),
            self.client.clone(),
            edit::Write {
                commit: Some(commit),
                preedit: String::new(),
                guard: None,
                allow_stale: true,
            },
        ) {
            log(&format!("失焦落定请求失败: {error}"));
        }
    }

    /// 焦点变化:失焦把敲了一半的拼音落定到最近上下文。
    /// `ITfKeyEventSink::OnSetFocus`(同应用换输入框)与线程焦点走同一条。
    fn focus_changed(&self, focused: bool) {
        self.shared.touch();
        self.shared.bump_context_epoch();
        self.pending.borrow_mut().take();
        // Caps Lock 可能在别的窗口打开/关闭;拿到焦点时让语言栏重新取图标。
        self.shared.refresh_langbar();
        if let Some(reply) = self.request(Request::Focus {
            session: self.shared.session,
            focused,
        }) {
            if focused {
                self.shared.set_chinese(reply.chinese);
            } else if let Some(context) = self.shared.last_context.borrow().clone() {
                self.apply_stale(&context, &reply);
            } else {
                self.shared.set_chinese(reply.chinese);
            }
        }
    }
}

impl Drop for TextService {
    fn drop(&mut self) {
        crate::unlock_module();
    }
}

/// 懒连接 + 退避 + Hello;语言栏按钮也走这条路,连接状态与会话号只有一份。
pub(crate) fn request_with(
    client: &SharedClient,
    shared: &Shared,
    req: Request,
) -> Option<Reply> {
    // Hello 的应答带全局中/英状态;先放开连接锁再写 compartment,
    // 因为 SetValue 可能同步回调 OnChange,回调里还要借同一个客户端。
    let hello_chinese = {
        let mut guard = client.borrow_mut();
        if guard.is_none() {
            let may_retry = shared
                .last_connect
                .get()
                .is_none_or(|t| t.elapsed() >= RETRY_INTERVAL);
            if may_retry {
                shared.last_connect.set(Some(Instant::now()));
                if let Some(mut new_client) = Client::connect() {
                    let hello = new_client.round_trip(&Request::Hello {
                        session: shared.session,
                        host: host_app_name(),
                    });
                    *guard = Some(new_client);
                    hello.map(|reply| reply.chinese)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(chinese) = hello_chinese {
        shared.set_chinese(chinese);
    }
    let mut guard = client.borrow_mut();
    let connection = guard.as_mut()?;
    let reply = connection.round_trip(&req);
    if reply.is_none() {
        // 断线:丢弃连接,下键重新尝试。
        *guard = None;
    }
    reply
}

/// 用最近一次按键的上下文写回应答(语言栏、系统模式切换走这条路)。
pub(crate) fn apply_reply(shared: &Rc<Shared>, client: &SharedClient, reply: &Reply) {
    shared.touch();
    shared.set_chinese(reply.chinese);
    let commit = reply.commits.concat();
    if commit.is_empty() && reply.preedit.is_empty() && !shared.composing() {
        return;
    }
    let Some(context) = shared.last_context.borrow().clone() else {
        return;
    };
    if let Err(error) = edit::request_update(
        &context,
        shared.tid.get(),
        shared.session,
        shared.clone(),
        Rc::clone(client),
        edit::Write {
            commit: (!commit.is_empty()).then_some(commit),
            preedit: reply.preedit.clone(),
            guard: None,
            allow_stale: false,
        },
    ) {
        log(&format!("模式切换写文档失败: {error}"));
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        let thread_mgr = ptim.ok()?.clone();
        let keystroke: ITfKeystrokeMgr = thread_mgr.cast()?;
        let sink: ITfKeyEventSink = self.to_interface();
        unsafe { keystroke.AdviseKeyEventSink(tid, &sink, true)? };

        let source: ITfSource = thread_mgr.cast()?;
        let focus_sink: ITfThreadFocusSink = self.to_interface();
        let unknown: windows::core::IUnknown = focus_sink.into();
        let cookie = unsafe { source.AdviseSink(&ITfThreadFocusSink::IID, &unknown)? };
        *self.source.borrow_mut() = Some(source);
        self.focus_cookie.set(cookie);

        self.client_id.set(tid);
        self.shared.tid.set(tid);
        *self.shared.thread_mgr.borrow_mut() = Some(thread_mgr.clone());
        // 激活时先把系统指示器对齐当前模式;Focus 回调随后再跟 Server 的全局值。
        self.shared.mode_writing.set(true);
        crate::mode::write_indicator(&thread_mgr, tid, self.shared.chinese.get());
        self.shared.mode_writing.set(false);
        *self.poll.borrow_mut() = crate::poll::PollTimer::new(
            self.client.clone(),
            self.shared.clone(),
            tid,
            self.shared.session,
        );
        *self.mode_advice.borrow_mut() = crate::mode::advise(
            &thread_mgr,
            self.shared.clone(),
            self.client.clone(),
        );
        if let Some(button) =
            crate::langbar::add(self.shared.clone(), self.client.clone(), &thread_mgr)
        {
            self.langbar.borrow_mut().replace(button);
        }
        *self.thread_mgr.borrow_mut() = Some(thread_mgr);
        log(&format!("激活 tid={tid} session={}", self.shared.session));
        Ok(())
    }

    fn Deactivate(&self) -> Result<()> {
        self.poll.borrow_mut().take();
        self.shared.bump_context_epoch();
        self.shared.langbar_sink.borrow_mut().take();
        // 停用可能发生在切换输入法/关闭应用时;先把敲了一半的拼音落定。
        self.pending.borrow_mut().take();
        if let Some(context) = self.shared.last_context.borrow().clone()
            && let Some(reply) = self.request(Request::Focus {
                session: self.shared.session,
                focused: false,
            })
        {
            self.apply(&context, &reply);
        }
        crate::mode::unadvise(std::mem::take(&mut *self.mode_advice.borrow_mut()));
        if let Some(button) = self.langbar.borrow_mut().take() {
            crate::langbar::remove(&button, self.thread_mgr.borrow().as_ref());
        }
        if let Some(source) = self.source.borrow().clone() {
            unsafe {
                let _ = source.UnadviseSink(self.focus_cookie.get());
            }
        }
        *self.source.borrow_mut() = None;
        if let Some(thread_mgr) = self.thread_mgr.borrow().clone()
            && let Ok(keystroke) = thread_mgr.cast::<ITfKeystrokeMgr>()
        {
            unsafe {
                let _ = keystroke.UnadviseKeyEventSink(self.client_id.get());
            }
        }
        *self.thread_mgr.borrow_mut() = None;
        *self.shared.thread_mgr.borrow_mut() = None;
        self.client_id.set(0);
        if let Some(mut client) = self.client.borrow_mut().take() {
            let _ = client.round_trip(&Request::Goodbye {
                session: self.shared.session,
            });
        }
        log("停用");
        Ok(())
    }
}

impl ITfThreadFocusSink_Impl for TextService_Impl {
    /// 线程焦点:切窗口时 ITfKeyEventSink::OnSetFocus 不触发,走这条。
    /// 失焦侧清组合(由 Server 落定),避免拼音串跟随到下一个窗口。
    fn OnSetThreadFocus(&self) -> Result<()> {
        let _ = catch_unwind(AssertUnwindSafe(|| self.focus_changed(true)));
        Ok(())
    }

    fn OnKillThreadFocus(&self) -> Result<()> {
        let _ = catch_unwind(AssertUnwindSafe(|| self.focus_changed(false)));
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    /// 同一个应用里换输入框时线程焦点不变,由这条通知处理;失焦先落定拼音。
    fn OnSetFocus(&self, fforeground: BOOL) -> Result<()> {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.focus_changed(fforeground.as_bool());
        }));
        Ok(())
    }

    /// 所有键先过这里:问 Server 吃不吃;直通的键立即放行。
    fn OnTestKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        // 任何意外 panic 都不能穿过 COM 边界拖垮宿主进程。
        match catch_unwind(AssertUnwindSafe(|| {
            let vk = wparam.0 as u32;
            self.tap.note_down(vk);
            // 上一个键没被消费时留下的应答不能落到这个键头上(例如语言栏切换后按 Ctrl+C)。
            let stale = self
                .pending
                .borrow()
                .as_ref()
                .is_some_and(|pending| pending.vk != vk);
            if stale {
                self.pending.borrow_mut().take();
            }
            if keys::is_shift(vk) || keys::is_caps(vk) {
                // 单击判定与模式同步都在 keyup。
                return Ok(FALSE);
            }
            let Some(key) = self.mapped_key(vk) else {
                return Ok(FALSE);
            };
            let Ok(context) = pic.ok() else {
                return Ok(FALSE);
            };
            // 密码框/空上下文:键全部放行,不组句。
            if crate::context::keyboard_disabled(context) {
                return Ok(FALSE);
            }
            let Some(reply) = self.request(Request::Key {
                session: self.shared.session,
                key,
            }) else {
                // Server 不在:全部放行(宁可英文直通,不能死键盘)。
                return Ok(FALSE);
            };
            // 英文模式下字母会直通,但语言栏仍要跟上全局模式。
            self.shared.set_chinese(reply.chinese);
            if reply.passthrough {
                return Ok(FALSE);
            }
            *self.shared.last_context.borrow_mut() = Some(context.clone());
            *self.pending.borrow_mut() = Some(Pending {
                context: context.clone(),
                reply,
                vk,
            });
            crate::context_probe::request(
                context,
                self.client_id.get(),
                self.shared.session,
                self.shared.clone(),
                self.client.clone(),
            );
            Ok(true.into())
        })) {
            Ok(result) => result,
            Err(_) => {
                log("按键预演 panic(已兜住)");
                Ok(FALSE)
            }
        }
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        match catch_unwind(AssertUnwindSafe(|| {
            let vk = wparam.0 as u32;
            // 密码框/空上下文:即使预演阶段留下了应答,也不能把组句/上屏写进去。
            if let Ok(context) = pic.ok()
                && crate::context::keyboard_disabled(context)
            {
                self.pending.borrow_mut().take();
                return Ok(FALSE);
            }
            let pending = {
                let mut slot = self.pending.borrow_mut();
                if slot.as_ref().is_some_and(|pending| pending.vk == vk) {
                    slot.take()
                } else {
                    None
                }
            };
            if let Some(pending) = pending {
                self.apply(&pending.context, &pending.reply);
                return Ok(true.into());
            }
            Ok(FALSE)
        })) {
            Ok(result) => result,
            Err(_) => {
                log("按键应用 panic(已兜住)");
                Ok(FALSE)
            }
        }
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(FALSE)
    }

    fn OnKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        match catch_unwind(AssertUnwindSafe(|| {
            let vk = wparam.0 as u32;
            // CapsLock:大写锁定 = 临时英文;灭灯后回到亮灯前的模式。
            if keys::is_caps(vk) {
                self.shared.touch();
                let caps = keys::caps_on();
                let chinese = if caps {
                    let previous = self
                        .caps_restore
                        .get()
                        .unwrap_or_else(|| self.shared.chinese.get());
                    self.caps_restore.set(Some(previous));
                    false
                } else {
                    self.caps_restore.take().unwrap_or(true)
                };
                if let Some(reply) = self.request(Request::SetMode {
                    session: self.shared.session,
                    chinese,
                }) {
                    // 组合中切大写要先把拼音落定。
                    if (!reply.commits.is_empty()
                        || !reply.preedit.is_empty()
                        || self.shared.composing())
                        && let Ok(context) = pic.ok()
                        && !crate::context::keyboard_disabled(&context)
                    {
                        *self.shared.last_context.borrow_mut() = Some(context.clone());
                        self.apply(&context, &reply);
                    } else {
                        self.shared.set_chinese(reply.chinese);
                    }
                }
                // 已经是英文时 SetMode 不改变 chinese,但图标要从「英」变成「A」。
                self.shared.refresh_langbar();
                return Ok(FALSE);
            }
            // Shift 单击:切中英(组合中先上屏再切,由 Server 引擎处理)。
            if self.tap.take_shift_tap(vk)
                && let Ok(context) = pic.ok()
                && !crate::context::keyboard_disabled(&context)
                && let Some(reply) = self.request(Request::Key {
                    session: self.shared.session,
                    key: pinyin_protocol::KeyMsg::Shift,
                })
            {
                *self.shared.last_context.borrow_mut() = Some(context.clone());
                self.apply(&context, &reply);
            }
            Ok(FALSE)
        })) {
            Ok(result) => result,
            Err(_) => {
                log("抬键处理 panic(已兜住)");
                Ok(FALSE)
            }
        }
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, _rguid: *const GUID) -> Result<BOOL> {
        Ok(FALSE)
    }
}

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
