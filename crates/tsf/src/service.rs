//! 文本服务主体:激活/停用、按键接收(TestKeyDown 推进引擎并缓存结果,KeyDown 写文档)、
//! 显示属性提供。引擎在 TestKeyDown 阶段推进一次,OnKeyDown 只应用文档更新,
//! 引擎说直通的键在 Test 就放行,不会出现「吃了又吞」。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use pinyin_engine::{Engine, Event, Key, Mode};
use windows::Win32::Foundation::{BOOL, E_FAIL, E_INVALIDARG, FALSE, LPARAM, WPARAM};
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, ITfComposition, ITfContext, ITfDisplayAttributeInfo,
    ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl, ITfKeyEventSink,
    ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfTextInputProcessor, ITfTextInputProcessor_Impl,
    ITfThreadMgr,
};
use windows::core::{Error, GUID, Interface, Ref, Result, implement};

use crate::attr;
use crate::edit;
use crate::engine_host;
use crate::keys::{self, KeyTap};
use crate::log;

/// 跨按键存活的组句状态(STA 单线程,Rc 传递)。
pub(crate) struct Shared {
    pub composition: RefCell<Option<ITfComposition>>,
}

/// TestKeyDown 缓存、待 KeyDown 写文档的结果。
struct Pending {
    context: ITfContext,
    events: Vec<Event>,
}

#[implement(ITfTextInputProcessor, ITfKeyEventSink, ITfDisplayAttributeProvider)]
pub(crate) struct TextService {
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    client_id: Cell<u32>,
    engine: Rc<RefCell<Engine>>,
    shared: Rc<Shared>,
    tap: KeyTap,
    pending: RefCell<Option<Pending>>,
}

impl TextService {
    pub(crate) fn new() -> Self {
        Self {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            engine: Rc::new(RefCell::new(engine_host::load_engine())),
            shared: Rc::new(Shared {
                composition: RefCell::new(None),
            }),
            tap: KeyTap::new(),
            pending: RefCell::new(None),
        }
    }

    fn mapped_key(&self, vk: u32) -> Option<Key> {
        if keys::command_mods() {
            return None;
        }
        keys::vk_to_key(vk, keys::shift_down())
    }

    /// 把缓存的按键结果写进文档(异步编辑会话)。
    fn apply(&self, context: &ITfContext, events: &[Event]) {
        let commit: String = events
            .iter()
            .filter_map(|e| match e {
                Event::Commit(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let preedit = self
            .engine
            .borrow()
            .composition()
            .map(|c| c.display.clone())
            .unwrap_or_default();
        if let Err(error) = edit::request_update(
            context,
            self.client_id.get(),
            self.shared.clone(),
            self.engine.clone(),
            (!commit.is_empty()).then_some(commit),
            preedit,
        ) {
            log(&format!("编辑会话请求失败: {error}"));
        }
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        let thread_mgr = ptim.ok().ok_or_else(|| Error::from(E_FAIL))?.clone();
        let keystroke: ITfKeystrokeMgr = thread_mgr.cast()?;
        let sink: ITfKeyEventSink = self.to_interface();
        unsafe { keystroke.AdviseKeyEventSink(tid, &sink, true)? };
        *self.thread_mgr.borrow_mut() = Some(thread_mgr);
        self.client_id.set(tid);
        log(&format!("激活 tid={tid}"));
        Ok(())
    }

    fn Deactivate(&self) -> Result<()> {
        if let Some(thread_mgr) = self.thread_mgr.borrow().clone() {
            if let Ok(keystroke) = thread_mgr.cast::<ITfKeystrokeMgr>() {
                unsafe {
                    let _ = keystroke.UnadviseKeyEventSink(self.client_id.get());
                }
            }
        }
        *self.thread_mgr.borrow_mut() = None;
        self.client_id.set(0);
        log("停用");
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, _fforeground: BOOL) -> Result<()> {
        Ok(())
    }

    /// 所有键先过这里:推进引擎、缓存结果;直通的键立即放行。
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
        let events = {
            let mut engine = self.engine.borrow_mut();
            engine_host::safe_process(&mut engine, key)
        };
        let eaten = !events.iter().any(|e| matches!(e, Event::Passthrough(_)));
        if eaten {
            *self.pending.borrow_mut() = Some(Pending {
                context: context.clone(),
                events,
            });
        }
        Ok(eaten.into())
    }

    fn OnKeyDown(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        if let Some(pending) = self.pending.borrow_mut().take() {
            self.apply(&pending.context, &pending.events);
            return Ok(true.into());
        }
        Ok(FALSE)
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(FALSE)
    }

    fn OnKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        let vk = wparam.0 as u32;
        // CapsLock:按大写锁定同步中英。
        if keys::is_caps(vk) {
            let mode = if keys::caps_on() {
                Mode::English
            } else {
                Mode::Chinese
            };
            let mut engine = self.engine.borrow_mut();
            engine_host::safe_process(&mut engine, Key::Esc);
            engine.set_mode(mode);
            return Ok(FALSE);
        }
        // Shift 单击:切中英(组合中先上屏再切)。
        if self.tap.take_shift_tap(vk)
            && let Ok(context) = pic.ok()
        {
            let events = {
                let mut engine = self.engine.borrow_mut();
                engine_host::safe_process(&mut engine, Key::Shift)
            };
            self.apply(context, &events);
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
