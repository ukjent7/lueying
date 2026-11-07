//! IMK 输入控制器:把按键交给引擎,把组合与候选窗同步给客户端。
//!
//! IMKServer 按 Info.plist 里的 `InputMethodServerControllerClass` 为每个
//! 客户端会话实例化一个控制器。控制器始终在主线程被调用。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lueying_engine::{Key, Mode};
use lueying_render::toolbar::{Action, BUTTONS, Status};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};
use objc2_foundation::{
    NSInteger, NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString, NSTimer, NSUInteger,
};
use objc2_input_method_kit::{IMKInputController, IMKServer, IMKStateSetting};

use crate::candidate::CandidatePanel;
use crate::session::Session;

/// 让 IMKServer 能按 Info.plist 里的类名从运行时找到本类。
pub fn register() {
    let _ = LueyingInputController::class();
}

/// macOS 虚拟键码;只列出输入法关心的部分。
const KEY_RETURN: u16 = 0x24;
const KEY_TAB: u16 = 0x30;
const KEY_DELETE: u16 = 0x33;
const KEY_ESCAPE: u16 = 0x35;
const KEY_SHIFT_LEFT: u16 = 0x38;
const KEY_SHIFT_RIGHT: u16 = 0x3c;
const KEY_ENTER: u16 = 0x4c;
const KEY_HOME: u16 = 0x73;
const KEY_PAGE_UP: u16 = 0x74;
const KEY_FORWARD_DELETE: u16 = 0x75;
const KEY_END: u16 = 0x77;
const KEY_PAGE_DOWN: u16 = 0x79;
const KEY_LEFT: u16 = 0x7b;
const KEY_RIGHT: u16 = 0x7c;
const KEY_DOWN: u16 = 0x7d;
const KEY_UP: u16 = 0x7e;

/// 停顿重排的静默窗口,与 Windows 侧一致。
const SETTLE_DELAY: f64 = 0.12;

#[derive(Default)]
struct ControllerIvars {
    session: RefCell<Option<Session>>,
    client: RefCell<Option<Retained<AnyObject>>>,
    panel: RefCell<Option<Rc<CandidatePanel>>>,
    settle_timer: RefCell<Option<Retained<NSTimer>>>,
    shift_down: Cell<bool>,
    shift_tapped: Cell<bool>,
}

define_class!(
    // SAFETY:IMKInputController 没有额外的子类约束;会话状态只在主线程访问。
    #[unsafe(super = IMKInputController)]
    #[thread_kind = MainThreadOnly]
    #[name = "LueyingInputController"]
    #[ivars = ControllerIvars]
    struct LueyingInputController;

    impl LueyingInputController {
        #[unsafe(method_id(initWithServer:delegate:client:))]
        fn init_with_server(
            this: Allocated<Self>,
            server: Option<&IMKServer>,
            delegate: Option<&AnyObject>,
            client: Option<&AnyObject>,
        ) -> Option<Retained<Self>> {
            let this = this.set_ivars(ControllerIvars::default());
            // SAFETY:父类初始化器签名一致;失败时返回 nil。
            let this: Option<Retained<Self>> = unsafe {
                msg_send![super(this), initWithServer: server, delegate: delegate, client: client]
            };
            this.inspect(|this| {
                if let Some(client) = client {
                    *this.ivars().client.borrow_mut() = Some(client.retain());
                }
                let mut session = Session::new();
                session.set_client(client);
                *this.ivars().session.borrow_mut() = Some(session);
            })
        }

        /// 接收按键与修饰键变化;返回 true 表示输入法消费了事件。
        #[unsafe(method(handleEvent:client:))]
        fn handle_event(&self, event: Option<&NSEvent>, client: Option<&AnyObject>) -> bool {
            // 宏会把 bool 返回值转成 Objective-C 的 BOOL,这里让所有
            // 提前返回都留在普通 Rust 函数里。
            self.handle_event_inner(event, client)
        }

        #[unsafe(method(commitComposition:))]
        fn commit_composition(&self, _sender: Option<&AnyObject>) {
            self.cancel_settle();
            if let Some(session) = self.ivars().session.borrow_mut().as_mut() {
                session.commit();
            }
            self.hide_panel();
        }

        #[unsafe(method(inputControllerWillClose))]
        fn input_controller_will_close(&self) {
            self.save_session();
            // 收掉候选窗,动画计时器随之作废,不再有迟到的回调。
            self.hide_panel();
        }

        /// 候选窗点击:kind 0=候选,1=上一页,2=下一页。
        #[unsafe(method(candidateClicked:kind:))]
        fn candidate_clicked(&self, index: NSInteger, kind: NSInteger) {
            {
                let mut sessions = self.ivars().session.borrow_mut();
                let Some(session) = sessions.as_mut() else {
                    return;
                };
                match kind {
                    0 => session.pick(index.max(0) as usize),
                    1 => session.page(-1),
                    2 => session.page(1),
                    _ => {}
                }
            }
            self.refresh_panel();
            self.schedule_settle();
        }

        /// 工具栏按钮的动作;编号是按钮在 `BUTTONS` 里的位置。
        #[unsafe(method(toolbarAction:))]
        fn toolbar_action(&self, index: NSInteger) {
            let button = usize::try_from(index).ok().and_then(|i| BUTTONS.get(i));
            if let Some(button) = button {
                self.run_toolbar_action(button.action());
            }
        }

        #[unsafe(method(settleTimer:))]
        fn settle_timer(&self, _timer: &NSTimer) {
            // 先取出计时器,借用结束后再释放它。
            let expired = self.ivars().settle_timer.borrow_mut().take();
            drop(expired);
            let active = {
                let mut sessions = self.ivars().session.borrow_mut();
                let Some(session) = sessions.as_mut() else {
                    return;
                };
                if session.view.raw.is_empty() {
                    false
                } else {
                    session.settle();
                    true
                }
            };
            if active {
                self.refresh_panel();
            }
        }

        /// 候选窗出现动画的逐帧回调;落定后候选窗自己作废计时器。
        #[unsafe(method(panelTick:))]
        fn panel_tick(&self, _timer: &NSTimer) {
            // 落定会释放计时器;先把 Rc 取出来,别在借用中触发连锁释放。
            let panel = self.ivars().panel.borrow().clone();
            if let Some(panel) = panel {
                panel.tick();
            }
        }
    }

    unsafe impl IMKStateSetting for LueyingInputController {
        #[unsafe(method(activateServer:))]
        fn activate_server(&self, sender: Option<&AnyObject>) {
            *self.ivars().client.borrow_mut() = sender.map(|client| client.retain());
            if let Some(session) = self.ivars().session.borrow_mut().as_mut() {
                session.set_client(sender);
                session.engine.clear_context();
            }
            self.show_toolbar();
        }

        #[unsafe(method(deactivateServer:))]
        fn deactivate_server(&self, _sender: Option<&AnyObject>) {
            self.cancel_settle();
            self.hide_panel();
            crate::toolbar::hide_for(self);
            if let Some(session) = self.ivars().session.borrow_mut().as_mut() {
                session.commit();
                session.save();
            }
            *self.ivars().client.borrow_mut() = None;
        }

        #[unsafe(method(recognizedEvents:))]
        fn recognized_events(&self, _sender: Option<&AnyObject>) -> NSUInteger {
            (NSEventMask::KeyDown | NSEventMask::FlagsChanged).0 as NSUInteger
        }
    }
);

impl LueyingInputController {
    fn handle_event_inner(&self, event: Option<&NSEvent>, client: Option<&AnyObject>) -> bool {
        let Some(event) = event else {
            return false;
        };
        if let Some(client) = client {
            *self.ivars().client.borrow_mut() = Some(client.retain());
        }
        if event.r#type() == NSEventType::FlagsChanged {
            return self.handle_flags(event);
        }
        if event.r#type() != NSEventType::KeyDown {
            return false;
        }
        self.ivars().shift_tapped.set(false);
        let Some(key) = key_of(event) else {
            return false;
        };
        let consumed = {
            let mut sessions = self.ivars().session.borrow_mut();
            let Some(session) = sessions.as_mut() else {
                return false;
            };
            session.process_key(key)
        };
        crate::toolbar::key_pressed();
        self.refresh_panel();
        self.schedule_settle();
        consumed
    }

    fn handle_flags(&self, event: &NSEvent) -> bool {
        let key_code = event.keyCode();
        if key_code != KEY_SHIFT_LEFT && key_code != KEY_SHIFT_RIGHT {
            return false;
        }
        let down = event.modifierFlags().contains(NSEventModifierFlags::Shift);
        let was_down = self.ivars().shift_down.replace(down);
        if down && !was_down {
            self.ivars().shift_tapped.set(true);
        } else if !down && was_down {
            let tapped = self.ivars().shift_tapped.replace(false);
            if tapped {
                let _ = self.process_engine_key(Key::Shift);
            }
        }
        true
    }

    fn process_engine_key(&self, key: Key) -> bool {
        let consumed = {
            let mut sessions = self.ivars().session.borrow_mut();
            let Some(session) = sessions.as_mut() else {
                return false;
            };
            session.process_key(key)
        };
        self.refresh_panel();
        self.schedule_settle();
        consumed
    }

    fn refresh_panel(&self) {
        self.refresh_toolbar();
        let view = {
            let sessions = self.ivars().session.borrow();
            let Some(session) = sessions.as_ref() else {
                return;
            };
            if session.view.raw.is_empty() {
                None
            } else {
                Some(session.view.clone())
            }
        };
        let Some(view) = view else {
            self.hide_panel();
            return;
        };
        let caret = self.caret_rect();
        self.panel().show(&view, caret);
    }

    fn panel(&self) -> Rc<CandidatePanel> {
        let mut slot = self.ivars().panel.borrow_mut();
        if slot.is_none() {
            let target: Retained<AnyObject> = self.retain().into();
            *slot = Some(Rc::new(CandidatePanel::new(self.mtm(), &target)));
        }
        Rc::clone(slot.as_ref().expect("候选窗刚刚创建"))
    }

    fn hide_panel(&self) {
        // 隐藏会作废动画计时器,先把 Rc 取出来再调用。
        let panel = self.ivars().panel.borrow().clone();
        if let Some(panel) = panel {
            panel.hide();
        }
    }

    fn show_toolbar(&self) {
        let target: Retained<AnyObject> = self.retain().into();
        crate::toolbar::show(self.mtm(), &target, self.toolbar_status());
    }

    /// 工具栏按钮的状态,跟当前会话走;没有会话时用默认值。
    fn toolbar_status(&self) -> Status {
        let sessions = self.ivars().session.borrow();
        match sessions.as_ref() {
            Some(session) => Status {
                chinese: session.engine.mode() == Mode::Chinese,
                fullwidth: session.engine.fullwidth(),
                punct: session.engine.config().chinese_punct,
                traditional: session.traditional,
            },
            None => Status {
                chinese: true,
                fullwidth: false,
                punct: true,
                traditional: false,
            },
        }
    }

    fn refresh_toolbar(&self) {
        crate::toolbar::update(self.toolbar_status());
    }

    fn run_toolbar_action(&self, action: Action) {
        match action {
            Action::ToggleMode => {
                self.process_engine_key(Key::Shift);
            }
            Action::ToggleFullWidth => self.with_session(Session::toggle_fullwidth),
            Action::TogglePunct => self.with_session(Session::toggle_punct),
            Action::ToggleScript => self.with_session(Session::toggle_script),
            Action::OpenSettings => crate::config::open_for_editing(),
            Action::Hide => crate::toolbar::collapse(),
        }
        self.refresh_toolbar();
    }

    /// 对当前会话做一次改动,然后刷新候选窗。
    fn with_session(&self, change: fn(&mut Session)) {
        if let Some(session) = self.ivars().session.borrow_mut().as_mut() {
            change(session);
        }
        self.refresh_panel();
    }

    fn caret_rect(&self) -> Option<NSRect> {
        let client = self.ivars().client.borrow();
        let client = client.as_deref()?;
        // SAFETY:respondsToSelector: 只查询方法是否存在。
        let responds: bool = unsafe {
            msg_send![
                client,
                respondsToSelector: sel!(attributesForCharacterIndex:lineHeightRectangle:)
            ]
        };
        if !responds {
            return None;
        }
        let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        // SAFETY:IMKTextInput 的客户端方法;index 0 表示当前选区。
        let _: *mut AnyObject = unsafe {
            msg_send![client, attributesForCharacterIndex: 0usize, lineHeightRectangle: &mut rect]
        };
        (rect.size.height > 0.0).then_some(rect)
    }

    fn schedule_settle(&self) {
        self.cancel_settle();
        let active = {
            let sessions = self.ivars().session.borrow();
            sessions
                .as_ref()
                .is_some_and(|session| !session.view.raw.is_empty())
        };
        if !active {
            return;
        }
        let target: Retained<AnyObject> = self.retain().into();
        // SAFETY:NSTimer 会保留 target;选择器在本类上已实现。
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                SETTLE_DELAY,
                &target,
                sel!(settleTimer:),
                None,
                false,
            )
        };
        *self.ivars().settle_timer.borrow_mut() = Some(timer);
    }

    fn cancel_settle(&self) {
        // 先取出,借用结束后再作废计时器。
        let timer = self.ivars().settle_timer.borrow_mut().take();
        if let Some(timer) = timer {
            timer.invalidate();
        }
    }

    fn save_session(&self) {
        if let Some(session) = self.ivars().session.borrow().as_ref() {
            session.save();
        }
    }
}

impl Drop for LueyingInputController {
    fn drop(&mut self) {
        if let Some(timer) = self.ivars().settle_timer.borrow_mut().take() {
            timer.invalidate();
        }
        if let Some(panel) = self.ivars().panel.borrow_mut().take() {
            panel.hide();
        }
        if let Some(session) = self.ivars().session.borrow_mut().take() {
            session.save();
        }
    }
}

/// 把 NSEvent 映射成平台无关的按键。
fn key_of(event: &NSEvent) -> Option<Key> {
    let flags = event.modifierFlags();
    if flags.contains(NSEventModifierFlags::Command) {
        return None;
    }
    if flags.contains(NSEventModifierFlags::Control) {
        return match event.keyCode() {
            KEY_DELETE => Some(Key::SegmentBackspace),
            KEY_FORWARD_DELETE => Some(Key::SegmentDelete),
            KEY_LEFT => Some(Key::SegmentLeft),
            KEY_RIGHT => Some(Key::SegmentRight),
            _ => None,
        };
    }
    if flags.contains(NSEventModifierFlags::Option) {
        return None;
    }
    let key = match event.keyCode() {
        KEY_RETURN | KEY_ENTER => Key::Enter,
        KEY_ESCAPE => Key::Esc,
        KEY_DELETE if flags.contains(NSEventModifierFlags::Shift) => Key::DeleteCandidate,
        KEY_DELETE => Key::Backspace,
        KEY_FORWARD_DELETE => Key::Delete,
        KEY_LEFT => Key::Left,
        KEY_RIGHT => Key::Right,
        KEY_DOWN => Key::Down,
        KEY_UP => Key::Up,
        KEY_HOME => Key::Home,
        KEY_END => Key::End,
        KEY_PAGE_UP => Key::PageUp,
        KEY_PAGE_DOWN => Key::PageDown,
        KEY_TAB => return None,
        _ => {
            let chars = event.characters()?;
            let ch = chars.to_string().chars().next()?;
            match ch {
                ' ' => Key::Space,
                c if c.is_ascii_alphabetic() => Key::Letter(c.to_ascii_lowercase()),
                c if c.is_ascii_digit() => Key::Digit(c.to_digit(10)? as u8),
                c if c.is_ascii() && !c.is_control() => Key::Punct(c),
                _ => return None,
            }
        }
    };
    Some(key)
}

/// 把文本插入客户端当前选区。
pub fn insert_text(client: &AnyObject, text: &str) {
    let text = NSString::from_str(text);
    // SAFETY:客户端实现了 IMKTextInput;NSNotFound 表示当前选区。
    let _: () = unsafe {
        msg_send![client, insertText: &*text, replacementRange: NSRange::new(NSNotFound as usize, 0)]
    };
}

/// 更新客户端的组合内联文本。
pub fn set_marked_text(client: &AnyObject, text: &str) {
    let text = NSString::from_str(text);
    let length = text.len_utf16();
    // SAFETY:selectionRange 相对传入字符串,replacementRange 表示当前选区。
    let _: () = unsafe {
        msg_send![
            client,
            setMarkedText: &*text,
            selectionRange: NSRange::new(length, 0),
            replacementRange: NSRange::new(NSNotFound as usize, 0)
        ]
    };
}

/// 清掉客户端的组合内联文本。
pub fn clear_marked_text(client: &AnyObject) {
    let empty = NSString::from_str("");
    // SAFETY:空 marked text 等价于结束内联组合。
    let _: () = unsafe {
        msg_send![
            client,
            setMarkedText: &*empty,
            selectionRange: NSRange::new(0, 0),
            replacementRange: NSRange::new(NSNotFound as usize, 0)
        ]
    };
}
