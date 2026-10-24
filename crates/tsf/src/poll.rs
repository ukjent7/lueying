//! 组句期间的停顿轮询:TSF 线程上挂一个 80ms 的 `WM_TIMER`,只在本线程组句时
//! 向 Server 拉一次当前内联文本。异步 25M 重排换了首选后,Server 不能直接写
//! 文档,由这里申请异步编辑会话把新首选写回;没有新按键也能保持一致。
//!
//! 定时器用线程定时器(`SetTimer(None, ...)`),回调仍在本 STA 消息泵上;
//! 宿主不泵消息时只是退回「下一键更新」,不会阻塞输入。

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use pinyin_protocol::Request;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::client::Client;
use crate::edit;
use crate::log;
use crate::service::{Shared, SharedClient};

const INTERVAL_MS: u32 = 80;

struct PollContext {
    client: SharedClient,
    shared: Rc<Shared>,
    client_id: u32,
    session: u64,
}

thread_local! {
    static ACTIVE: RefCell<Option<Rc<PollContext>>> = const { RefCell::new(None) };
}

pub(crate) struct PollTimer {
    id: usize,
}

impl PollTimer {
    pub(crate) fn new(
        client: SharedClient,
        shared: Rc<Shared>,
        client_id: u32,
        session: u64,
    ) -> Option<Self> {
        let id = unsafe { SetTimer(None, 0, INTERVAL_MS, Some(timer_proc)) };
        if id == 0 {
            log("停顿轮询定时器创建失败,退回按键时更新");
            return None;
        }
        ACTIVE.with(|active| {
            *active.borrow_mut() = Some(Rc::new(PollContext {
                client,
                shared,
                client_id,
                session,
            }));
        });
        Some(Self { id })
    }
}

impl Drop for PollTimer {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.borrow_mut().take());
        unsafe {
            let _ = KillTimer(None, self.id);
        }
    }
}

unsafe extern "system" fn timer_proc(_hwnd: HWND, _msg: u32, _id: usize, _time: u32) {
    let context = ACTIVE.with(|active| active.borrow().clone());
    if let Some(context) = context {
        let _ = catch_unwind(AssertUnwindSafe(|| poll_once(&context)));
    }
}

fn poll_once(context: &PollContext) {
    if !context.shared.composing() {
        return;
    }
    let Ok(mut guard) = context.client.try_borrow_mut() else {
        return;
    };
    let Some(client) = guard.as_mut() else {
        return;
    };
    let reply = match client.round_trip(&Request::Sync {
        session: context.session,
    }) {
        Some(reply) => reply,
        None => {
            *guard = None;
            return;
        }
    };
    drop(guard);
    if reply.preedit == context.shared.preedit.borrow().as_str() {
        return;
    }
    let revision = context.shared.revision.get();
    let base = context.shared.preedit.borrow().clone();
    let Some(document) = context.shared.last_context.borrow().clone() else {
        return;
    };
    let client: Rc<RefCell<Option<Client>>> = Rc::clone(&context.client);
    if let Err(error) = edit::request_update(
        &document,
        context.client_id,
        context.session,
        Rc::clone(&context.shared),
        client,
        None,
        reply.preedit,
        Some(revision),
        Some(base),
    ) {
        log(&format!("停顿重排写回文档失败: {error}"));
    }
}
