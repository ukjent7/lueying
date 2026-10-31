//! 键事件汇:焦点切换、按键下传与停顿泵的 TSF 接入。
//!
//! `service` 的 `#[path]` 子模块(src 保持扁平);`TextService_Impl`
//! 的私有字段经 `super::*` 取用,可见性与调用路径不变。

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::TextServices::ITfKeyEventSink_Impl;

use super::*;

impl ITfKeyEventSink_Impl for TextService_Impl {
    /// 同一个应用里换输入框时线程焦点不变,由这条通知处理;失焦先落定拼音。
    fn OnSetFocus(&self, fforeground: BOOL) -> Result<()> {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.focus_changed(fforeground.as_bool());
        }));
        Ok(())
    }

    /// 所有键先过这里:问 Server 吃不吃;直通的键立即放行。
    fn OnTestKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        // 任何意外 panic 都不能穿过 COM 边界拖垮宿主进程。
        match catch_unwind(AssertUnwindSafe(|| {
            let vk = wparam.0 as u32;
            // 字符键按物理扫描码 + 当前布局翻译。
            let scan = ((lparam.0 >> 16) & 0xff) as u32;
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
            // Shift+Space 切全/半角:与微软拼音、水杉一致,不进入组句。
            if !self.secure.get() && vk == 0x20 && keys::shift_down() && !keys::command_mods() {
                let Ok(context) = pic.ok() else {
                    return Ok(FALSE);
                };
                if crate::context::keyboard_disabled(context) {
                    return Ok(FALSE);
                }
                let Some(reply) = self.request(Request::ToggleFullwidth {
                    session: self.shared.session,
                }) else {
                    return Ok(FALSE);
                };
                self.shared.set_chinese(reply.chinese);
                self.shared.set_fullwidth(reply.fullwidth);
                *self.shared.last_context.borrow_mut() = Some(context.clone());
                *self.pending.borrow_mut() = Some(Pending {
                    context: context.clone(),
                    reply,
                    vk,
                    consume_only: true,
                });
                return Ok(true.into());
            }
            let Some(key) = self.mapped_key(vk, scan) else {
                return Ok(FALSE);
            };
            let Ok(context) = pic.ok() else {
                return Ok(FALSE);
            };
            // 密码框/空上下文:键全部放行,不组句。
            if crate::context::keyboard_disabled(context) {
                return Ok(FALSE);
            }
            // 激活时可能还没有焦点;第一个按键到达时补挂当前输入框的布局监听。
            let stale = self
                .layout
                .borrow()
                .as_ref()
                .is_none_or(|(ctx, _, _)| ctx.as_raw() != context.as_raw());
            if stale {
                self.unadvise_layout();
                self.advise_layout_context(context);
            }
            let Some(reply) = self.request(Request::Key {
                session: self.shared.session,
                key,
            }) else {
                // Server 不在:全部放行(宁可英文直通,不能死键盘)。
                return Ok(FALSE);
            };
            log(&format!(
                "预演 vk={vk} 直通={} 中文={} 上屏={:?} 内联={:?}",
                reply.passthrough, reply.chinese, reply.commits, reply.preedit
            ));
            // 英文模式下字母会直通,但语言栏仍要跟上全局模式。
            self.shared.set_chinese(reply.chinese);
            self.shared.set_fullwidth(reply.fullwidth);
            if reply.passthrough {
                return Ok(FALSE);
            }
            *self.shared.last_context.borrow_mut() = Some(context.clone());
            *self.pending.borrow_mut() = Some(Pending {
                context: context.clone(),
                reply,
                vk,
                consume_only: false,
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
                // 切全角只消费 Shift+Space,不改文档;组句中的内联文本保持原样。
                if pending.consume_only {
                    return Ok(true.into());
                }
                log(&format!(
                    "应用 vk={vk} 上屏={:?} 内联={:?}",
                    pending.reply.commits, pending.reply.preedit
                ));
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

    fn OnTestKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        // 预演阶段对每个键都会调用;Shift/Caps 只在这里能收到抬起。
        self.key_up(pic, wparam.0 as u32)
    }

    fn OnKeyUp(&self, pic: Ref<ITfContext>, wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        // 被吃掉的键在应用阶段还会经过这里。
        self.key_up(pic, wparam.0 as u32)
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, rguid: *const GUID) -> Result<BOOL> {
        if rguid.is_null() || unsafe { *rguid } != GUID_PRESERVED_PUNCT {
            return Ok(FALSE);
        }
        let handled = catch_unwind(AssertUnwindSafe(|| {
            // 开关写回配置并让 Server 立即生效;组件组合不受影响。
            log("保留键 Ctrl+.");
            self.request(Request::TogglePunct).is_some()
        }))
        .unwrap_or(false);
        // Server 不在时把键还给应用,不能静默吞掉。
        Ok(handled.into())
    }
}
