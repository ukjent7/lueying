//! TSF 候选 UI 元素:把当前页候选交给宿主/读屏软件。
//!
//! 桌面程序一般回 `BeginUIElement(TRUE)`,候选仍由服务进程的候选窗绘制;
//! UWP、全屏游戏、部分搜索框会回 `FALSE` 接管绘制,此时通知 Server 收起
//! 自己的窗口,由宿主经 `ITfCandidateListUIElement` 取候选。这个对象同时
//! 让 Narrator/NVDA 不必依赖一个无焦点的置顶窗口。

use std::rc::Rc;

use lueying_protocol::{Reply, Request};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::UI::TextServices::{
    ITfCandidateListUIElement, ITfCandidateListUIElementBehavior,
    ITfCandidateListUIElementBehavior_Impl, ITfCandidateListUIElement_Impl, ITfDocumentMgr,
    ITfUIElement, ITfUIElementMgr, ITfUIElement_Impl, TF_CLUIE_COUNT, TF_CLUIE_CURRENTPAGE,
    TF_CLUIE_DOCUMENTMGR, TF_CLUIE_PAGEINDEX, TF_CLUIE_SELECTION, TF_CLUIE_STRING,
};
use windows::core::{BSTR, BOOL, Error, GUID, Interface, Result, implement};

use crate::log;
use crate::service::{Shared, SharedClient, request_with};

pub(crate) const UI_ID_NONE: u32 = u32::MAX;

/// 候选 UI 元素的稳定 GUID;TSF 用它区分本输入法的候选窗。
const GUID_CANDIDATE_UI: GUID = GUID::from_u128(0x4d5e6f70_8192_4a3b_8c1d_2e3f40516273);

/// 与 Server 应答同步的候选快照。元素 getter 在同一 STA 线程读取。
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) candidates: Vec<String>,
    pub(crate) highlight: usize,
    pub(crate) visible: bool,
}

/// 每个按键应答/停顿轮询后调用:组合中登记或刷新候选元素,组合结束则撤销。
pub(crate) fn sync(shared: &Rc<Shared>, client: &SharedClient, reply: &Reply) {
    let active = !reply.preedit.is_empty() && !reply.candidates.is_empty();
    {
        let mut snapshot = shared.ui_snapshot.borrow_mut();
        snapshot.candidates.clone_from(&reply.candidates);
        snapshot.highlight = reply.highlight;
        snapshot.visible = active;
    }
    if !active {
        end(shared, client);
        return;
    }
    let id = shared.ui_id.get();
    if id == UI_ID_NONE {
        begin(shared, client);
    } else if let Some(mgr) = shared.ui_mgr.borrow().as_ref() {
        unsafe {
            let _ = mgr.UpdateUIElement(id);
        }
    }
}

/// 组合结束、失焦或停用时撤销元素,并把候选窗所有权还给 Server。
pub(crate) fn end(shared: &Rc<Shared>, client: &SharedClient) {
    let id = shared.ui_id.replace(UI_ID_NONE);
    if id != UI_ID_NONE
        && let Some(mgr) = shared.ui_mgr.borrow().as_ref()
    {
        unsafe {
            let _ = mgr.EndUIElement(id);
        }
    }
    set_host_draws(shared, client, false);
    let mut snapshot = shared.ui_snapshot.borrow_mut();
    snapshot.visible = false;
    snapshot.candidates.clear();
}

fn begin(shared: &Rc<Shared>, client: &SharedClient) {
    let Some(mgr) = shared.ui_mgr.borrow().clone() else {
        return;
    };
    let element: ITfCandidateListUIElementBehavior = CandidateElement {
        shared: Rc::clone(shared),
        client: Rc::clone(client),
    }
    .into();
    let Ok(ui) = element.cast::<ITfUIElement>() else {
        return;
    };
    let mut show = BOOL::from(true);
    let mut id = 0u32;
    match unsafe { mgr.BeginUIElement(&ui, &mut show, &mut id) } {
        Ok(()) => {
            shared.ui_id.set(id);
            // 宿主回 FALSE 表示它自己绘制候选,服务端窗口收起。
            set_host_draws(shared, client, !show.as_bool());
        }
        Err(error) => log(&format!("登记候选 UI 元素失败: {error}")),
    }
}

fn set_host_draws(shared: &Rc<Shared>, client: &SharedClient, host_draws: bool) {
    if shared.ui_host_draws.replace(host_draws) == host_draws {
        return;
    }
    let _ = request_with(
        client,
        shared,
        Request::UiMode {
            session: shared.session,
            host_draws,
        },
    );
}

#[implement(
    ITfUIElement,
    ITfCandidateListUIElement,
    ITfCandidateListUIElementBehavior
)]
struct CandidateElement {
    shared: Rc<Shared>,
    client: SharedClient,
}

impl ITfUIElement_Impl for CandidateElement_Impl {
    fn GetDescription(&self) -> Result<BSTR> {
        Ok(BSTR::from("掠影候选"))
    }

    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_CANDIDATE_UI)
    }

    fn Show(&self, bshow: BOOL) -> Result<()> {
        let show = bshow.as_bool();
        self.shared.ui_snapshot.borrow_mut().visible = show;
        // 与 BeginUIElement 的返回值同义:宿主隐藏元素即接管绘制。
        set_host_draws(&self.shared, &self.client, !show);
        Ok(())
    }

    fn IsShown(&self) -> Result<BOOL> {
        Ok(BOOL::from(self.shared.ui_snapshot.borrow().visible))
    }
}

impl ITfCandidateListUIElement_Impl for CandidateElement_Impl {
    fn GetUpdatedFlags(&self) -> Result<u32> {
        Ok(TF_CLUIE_DOCUMENTMGR
            | TF_CLUIE_COUNT
            | TF_CLUIE_SELECTION
            | TF_CLUIE_STRING
            | TF_CLUIE_PAGEINDEX
            | TF_CLUIE_CURRENTPAGE)
    }

    fn GetDocumentMgr(&self) -> Result<ITfDocumentMgr> {
        let thread_mgr = self
            .shared
            .thread_mgr
            .borrow()
            .clone()
            .ok_or_else(|| Error::from(E_FAIL))?;
        unsafe { thread_mgr.GetFocus() }
    }

    fn GetCount(&self) -> Result<u32> {
        Ok(self.shared.ui_snapshot.borrow().candidates.len() as u32)
    }

    fn GetSelection(&self) -> Result<u32> {
        let snapshot = self.shared.ui_snapshot.borrow();
        Ok(snapshot
            .highlight
            .min(snapshot.candidates.len().saturating_sub(1)) as u32)
    }

    fn GetString(&self, uindex: u32) -> Result<BSTR> {
        let snapshot = self.shared.ui_snapshot.borrow();
        let text = snapshot
            .candidates
            .get(uindex as usize)
            .cloned()
            .ok_or_else(|| Error::from(E_FAIL))?;
        Ok(BSTR::from(text))
    }

    fn GetPageIndex(&self, pindex: *mut u32, size: u32, pupagecnt: *mut u32) -> Result<()> {
        unsafe {
            if pupagecnt.is_null() {
                return Err(Error::from(E_FAIL));
            }
            // 当前只把一页候选交给宿主;分页由服务端候选窗负责。
            *pupagecnt = 1;
            if !pindex.is_null() && size > 0 {
                *pindex = 0;
            }
        }
        Ok(())
    }

    fn SetPageIndex(&self, _pindex: *const u32, _upagecnt: u32) -> Result<()> {
        Ok(())
    }

    fn GetCurrentPage(&self) -> Result<u32> {
        Ok(0)
    }
}

impl ITfCandidateListUIElementBehavior_Impl for CandidateElement_Impl {
    fn SetSelection(&self, nindex: u32) -> Result<()> {
        self.shared.ui_snapshot.borrow_mut().highlight = nindex as usize;
        Ok(())
    }

    fn Finalize(&self) -> Result<()> {
        let index = self.shared.ui_snapshot.borrow().highlight;
        let _ = request_with(
            &self.client,
            &self.shared,
            Request::PickIndex {
                session: self.shared.session,
                index,
            },
        );
        self.shared.ui_snapshot.borrow_mut().visible = false;
        Ok(())
    }

    fn Abort(&self) -> Result<()> {
        let _ = request_with(
            &self.client,
            &self.shared,
            Request::Key {
                session: self.shared.session,
                key: lueying_protocol::KeyMsg::Esc,
            },
        );
        self.shared.ui_snapshot.borrow_mut().visible = false;
        Ok(())
    }
}
