//! IMK 输入控制器:把按键交给引擎,把组合与候选窗同步给客户端。
//!
//! IMKServer 按 Info.plist 里的 `InputMethodServerControllerClass` 为每个
//! 客户端会话实例化一个控制器。控制器始终在主线程被调用。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lueying_engine::Key;
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

struct ControllerIvars {
    session: RefCell<Option<Session>>,
    client: RefCell<Option<Retained<AnyObject>>>,
    panel: RefCell<Option<Rc<CandidatePanel>>>,
    settle_timer: RefCell<Option<Retained<NSTimer>>>,
    shift_down: Cell<bool>,
    shift_tapped: Cell<bool>,
}

impl Default for ControllerIvars {
    fn default() -> Self {
        Self {
            session: RefCell::new(None),
            client: RefCell::new(None),
            panel: RefCell::new(None),
            settle_timer: RefCell::new(None),
            shift_down: Cell::new(false),
            shift_tapped: Cell::new(false),
        }
    }
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

        #[unsafe(method(settleTimer:))]
        fn settle_timer(&self, _timer: &NSTimer) {
            *self.ivars().settle_timer.borrow_mut() = None;
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
    }

    unsafe impl IMKStateSetting for LueyingInputController {
        #[unsafe(method(activateServer:))]
        fn activate_server(&self, sender: Option<&AnyObject>) {
            *self.ivars().client.borrow_mut() = sender.map(|client| client.retain());
            if let Some(session) = self.ivars().session.borrow_mut().as_mut() {
                session.set_client(sender);
                session.engine.clear_context();
            }
        }

        #[unsafe(method(deactivateServer:))]
        fn deactivate_server(&self, _sender: Option<&AnyObject>) {
            self.cancel_settle();
            self.hide_panel();
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
        if let Some(panel) = self.ivars().panel.borrow().as_ref() {
            panel.hide();
        }
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
        if let Some(timer) = self.ivars().settle_timer.borrow_mut().take() {
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
