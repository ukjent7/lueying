//! 文本服务主体:激活/停用、按键接收、焦点通知、显示属性提供。
//! 键在 TestKeyDown 阶段经管道送 Server 预演并缓存应答,OnKeyDown 只应用文档更新;
//! Server 不在时全部放行(字母直通,防死键盘)。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use pinyin_protocol::{Reply, Request};
use windows::Win32::Foundation::{E_INVALIDARG, FALSE, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, ITfComposition, ITfContext, ITfDisplayAttributeInfo,
    ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl, ITfKeyEventSink,
    ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource, ITfTextInputProcessor,
    ITfTextInputProcessor_Impl, ITfThreadFocusSink, ITfThreadFocusSink_Impl, ITfThreadMgr,
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
}

pub(crate) type SharedClient = Rc<RefCell<Option<Client>>>;

/// TestKeyDown 缓存、待 KeyDown 写文档的应答。
struct Pending {
    context: ITfContext,
    reply: Reply,
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
    /// 会话号:宿主线程 id(TSF client id 各进程都一样,会撞号)。
    session: u64,
    client: SharedClient,
    shared: Rc<Shared>,
    tap: KeyTap,
    pending: RefCell<Option<Pending>>,
    last_connect: Cell<Option<Instant>>,
    source: RefCell<Option<ITfSource>>,
    focus_cookie: Cell<u32>,
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
        Self {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            session: u64::from(unsafe { GetCurrentThreadId() }),
            client: Rc::new(RefCell::new(None)),
            shared: Rc::new(Shared {
                composition: RefCell::new(None),
                preedit: RefCell::new(String::new()),
                last_context: RefCell::new(None),
                revision: Cell::new(0),
                probing: Cell::new(false),
                context_epoch: Cell::new(0),
            }),
            tap: KeyTap::new(),
            pending: RefCell::new(None),
            last_connect: Cell::new(None),
            source: RefCell::new(None),
            focus_cookie: Cell::new(0),
            poll: RefCell::new(None),
        }
    }

    fn mapped_key(&self, vk: u32) -> Option<pinyin_protocol::KeyMsg> {
        if keys::command_mods() {
            return None;
        }
        keys::vk_to_key(vk, keys::shift_down())
    }

    /// 发请求(懒连接 + 退避);连上时先 Hello。
    fn request(&self, req: Request) -> Option<Reply> {
        let mut guard = self.client.borrow_mut();
        if guard.is_none() {
            let may_retry = self
                .last_connect
                .get()
                .is_none_or(|t| t.elapsed() >= RETRY_INTERVAL);
            if may_retry {
                self.last_connect.set(Some(Instant::now()));
                if let Some(mut client) = Client::connect() {
                    let _ = client.round_trip(&Request::Hello {
                        session: self.session,
                        host: host_app_name(),
                    });
                    *guard = Some(client);
                }
            }
        }
        let client = guard.as_mut()?;
        let reply = client.round_trip(&req);
        if reply.is_none() {
            // 断线:丢弃连接,下键重新尝试。
            *guard = None;
        }
        reply
    }

    /// 把应答写进文档(异步编辑会话,会话里量光标报给 Server 摆候选窗)。
    fn apply(&self, context: &ITfContext, reply: &Reply) {
        // 新按键让任何在途的停顿写回失效。
        self.shared.touch();
        let commit: String = reply.commits.concat();
        if let Err(error) = edit::request_update(
            context,
            self.client_id.get(),
            self.session,
            self.shared.clone(),
            self.client.clone(),
            edit::Write {
                commit: (!commit.is_empty()).then_some(commit),
                preedit: reply.preedit.clone(),
                guard: None,
            },
        ) {
            log(&format!("编辑会话请求失败: {error}"));
        }
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

        *self.thread_mgr.borrow_mut() = Some(thread_mgr);
        self.client_id.set(tid);
        *self.poll.borrow_mut() = crate::poll::PollTimer::new(
            self.client.clone(),
            self.shared.clone(),
            tid,
            self.session,
        );
        log(&format!("激活 tid={tid} session={}", self.session));
        Ok(())
    }

    fn Deactivate(&self) -> Result<()> {
        self.poll.borrow_mut().take();
        self.shared.bump_context_epoch();
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
        self.client_id.set(0);
        if let Some(mut client) = self.client.borrow_mut().take() {
            let _ = client.round_trip(&Request::Goodbye {
                session: self.session,
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
        self.shared.touch();
        self.shared.bump_context_epoch();
        let _ = self.request(Request::Focus {
            session: self.session,
            focused: true,
        });
        Ok(())
    }

    fn OnKillThreadFocus(&self) -> Result<()> {
        self.shared.touch();
        self.shared.bump_context_epoch();
        let _ = self.request(Request::Focus {
            session: self.session,
            focused: false,
        });
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, _fforeground: BOOL) -> Result<()> {
        Ok(())
    }

    /// 所有键先过这里:问 Server 吃不吃;直通的键立即放行。
    fn OnTestKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        let vk = wparam.0 as u32;
        self.tap.note_down(vk);
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
            session: self.session,
            key,
        }) else {
            // Server 不在:全部放行(宁可英文直通,不能死键盘)。
            return Ok(FALSE);
        };
        if reply.passthrough {
            return Ok(FALSE);
        }
        *self.shared.last_context.borrow_mut() = Some(context.clone());
        *self.pending.borrow_mut() = Some(Pending {
            context: context.clone(),
            reply,
        });
        crate::context_probe::request(
            &context,
            self.client_id.get(),
            self.session,
            self.shared.clone(),
            self.client.clone(),
        );
        Ok(true.into())
    }

    fn OnKeyDown(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        if let Some(pending) = self.pending.borrow_mut().take() {
            self.apply(&pending.context, &pending.reply);
            return Ok(true.into());
        }
        Ok(FALSE)
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(FALSE)
    }

    fn OnKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        let vk = wparam.0 as u32;
        // CapsLock:大写锁定 = 临时英文。
        if keys::is_caps(vk) {
            self.shared.touch();
            let _ = self.request(Request::SetMode {
                session: self.session,
                chinese: !keys::caps_on(),
            });
            return Ok(FALSE);
        }
        // Shift 单击:切中英(组合中先上屏再切,由 Server 引擎处理)。
        if self.tap.take_shift_tap(vk)
            && let Ok(context) = pic.ok()
            && let Some(reply) = self.request(Request::Key {
                session: self.session,
                key: pinyin_protocol::KeyMsg::Shift,
            })
        {
            self.apply(context, &reply);
        }
        Ok(FALSE)
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
        if unsafe { *guid } == attr::GUID_ATTR_INPUT {
            Ok(attr::info())
        } else {
            Err(Error::from(E_INVALIDARG))
        }
    }
}
