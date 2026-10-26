//! TSF 候选 UI 元素:把整份候选与分页交给宿主/读屏软件。
//!
//! 桌面程序一般回 `BeginUIElement(TRUE)`,候选仍由服务进程的候选窗绘制;
//! UWP、全屏游戏、部分搜索框会回 `FALSE` 接管绘制,此时通知 Server 收起
//! 自己的窗口,由宿主经 `ITfCandidateListUIElement` 取候选。这个对象同时
//! 让 Narrator/NVDA 不必依赖一个无焦点的置顶窗口。

use std::rc::Rc;

use lueying_protocol::{Reply, Request};
use windows::Win32::Foundation::{E_FAIL, LPARAM, WPARAM};
use windows::Win32::UI::TextServices::{
    ITfCandidateListUIElement, ITfCandidateListUIElement_Impl, ITfCandidateListUIElementBehavior,
    ITfCandidateListUIElementBehavior_Impl, ITfDocumentMgr, ITfIntegratableCandidateListUIElement,
    ITfIntegratableCandidateListUIElement_Impl, ITfUIElement, ITfUIElement_Impl, ITfUIElementMgr,
    STYLE_ACTIVE_SELECTION, TF_CLUIE_COUNT, TF_CLUIE_CURRENTPAGE, TF_CLUIE_DOCUMENTMGR,
    TF_CLUIE_PAGEINDEX, TF_CLUIE_SELECTION, TF_CLUIE_STRING,
    TfIntegratableCandidateListSelectionStyle,
};
use windows::core::{BOOL, BSTR, Error, GUID, Interface, Result, implement};

use crate::log;
use crate::service::{Shared, SharedClient, apply_reply, request_with};

pub(crate) const UI_ID_NONE: u32 = u32::MAX;

/// 候选 UI 元素的稳定 GUID;TSF 用它区分本输入法的候选窗。
const GUID_CANDIDATE_UI: GUID = GUID::from_u128(0x4d5e6f70_8192_4a3b_8c1d_2e3f40516273);

/// 与 Server 应答同步的候选快照。元素 getter 在同一 STA 线程读取。
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) candidates: Vec<String>,
    pub(crate) highlight: usize,
    pub(crate) page_size: usize,
    pub(crate) visible: bool,
}

/// 每个按键应答/停顿轮询后调用:组合中登记或刷新候选元素,组合结束则撤销。
pub(crate) fn sync(shared: &Rc<Shared>, client: &SharedClient, reply: &Reply) {
    let active = !reply.preedit.is_empty() && !reply.candidates.is_empty();
    let page_size = reply.page_size.max(1);
    let changed = {
        let mut snapshot = shared.ui_snapshot.borrow_mut();
        let changed = snapshot.candidates != reply.candidates
            || snapshot.highlight != reply.highlight
            || snapshot.page_size != page_size
            || snapshot.visible != active;
        snapshot.candidates.clone_from(&reply.candidates);
        snapshot.highlight = reply.highlight;
        snapshot.page_size = page_size;
        snapshot.visible = active;
        changed
    };
    if !active {
        end(shared, client);
        return;
    }
    let id = shared.ui_id.get();
    if id == UI_ID_NONE {
        begin(shared, client);
    } else if changed && let Some(mgr) = shared.ui_mgr.borrow().as_ref() {
        // 40ms 轮询会带同一份快照;没变就不打扰宿主/读屏软件。
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
    ITfCandidateListUIElementBehavior,
    ITfIntegratableCandidateListUIElement
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
            let snapshot = self.shared.ui_snapshot.borrow();
            let page_size = snapshot.page_size.max(1);
            let count = snapshot.candidates.len();
            let pages = count.div_ceil(page_size);
            *pupagecnt = pages as u32;
            if !pindex.is_null() {
                // 每一页的起始候选序号;宿主按这个数组分页显示。
                for index in 0..(size as usize).min(pages) {
                    *pindex.add(index) = (index * page_size) as u32;
                }
            }
        }
        Ok(())
    }

    fn SetPageIndex(&self, _pindex: *const u32, _upagecnt: u32) -> Result<()> {
        // 分页由服务端候选数设置决定,列表存续期间不改切法(Weasel 同款)。
        Ok(())
    }

    fn GetCurrentPage(&self) -> Result<u32> {
        let snapshot = self.shared.ui_snapshot.borrow();
        let page_size = snapshot.page_size.max(1);
        Ok((snapshot.highlight / page_size) as u32)
    }
}

impl ITfCandidateListUIElementBehavior_Impl for CandidateElement_Impl {
    fn SetSelection(&self, nindex: u32) -> Result<()> {
        let index = nindex as usize;
        if index >= self.shared.ui_snapshot.borrow().candidates.len() {
            return Err(Error::from(E_FAIL));
        }
        self.shared.ui_snapshot.borrow_mut().highlight = index;
        // 把高亮同步给服务端:候选窗(若可见)与随后的 Finalize 用同一项。
        if let Some(reply) = request_with(
            &self.client,
            &self.shared,
            Request::SelectIndex {
                session: self.shared.session,
                index,
            },
        ) {
            // 把高亮对应的内联预编辑也写回文档,候选列表与编辑框保持同一项。
            apply_reply(&self.shared, &self.client, &reply);
        }
        Ok(())
    }

    fn Finalize(&self) -> Result<()> {
        let index = self.shared.ui_snapshot.borrow().highlight;
        if let Some(reply) = request_with(
            &self.client,
            &self.shared,
            Request::PickIndex {
                session: self.shared.session,
                index,
            },
        ) {
            // 宿主自绘时上屏文本由 TSF 自己写进文档;光选中不够。
            apply_reply(&self.shared, &self.client, &reply);
        }
        self.shared.ui_snapshot.borrow_mut().visible = false;
        Ok(())
    }

    fn Abort(&self) -> Result<()> {
        if let Some(reply) = request_with(
            &self.client,
            &self.shared,
            Request::Key {
                session: self.shared.session,
                key: lueying_protocol::KeyMsg::Esc,
            },
        ) {
            apply_reply(&self.shared, &self.client, &reply);
        }
        self.shared.ui_snapshot.borrow_mut().visible = false;
        Ok(())
    }
}

impl ITfIntegratableCandidateListUIElement_Impl for CandidateElement_Impl {
    fn SetIntegrationStyle(&self, _guid: &GUID) -> Result<()> {
        // Windows 搜索框等宿主用它声明候选样式;当前只有一种内嵌列表样式。
        Ok(())
    }

    fn GetSelectionStyle(&self) -> Result<TfIntegratableCandidateListSelectionStyle> {
        // 高亮项就是空格/回车将上屏的那一项,与桌面候选窗一致。
        Ok(STYLE_ACTIVE_SELECTION)
    }

    fn OnKeyDown(&self, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        // 键的权威处理在 ITfKeyEventSink;这里吃键会把宿主导航键吞掉。
        Ok(false.into())
    }

    fn ShowCandidateNumbers(&self) -> Result<BOOL> {
        Ok(true.into())
    }

    fn FinalizeExactCompositionString(&self) -> Result<()> {
        // 搜索框要求按当前显示文本原样落定,不补全剩余拼音,也不改选候选。
        if let Some(reply) = request_with(
            &self.client,
            &self.shared,
            Request::Finalize {
                session: self.shared.session,
            },
        ) {
            apply_reply(&self.shared, &self.client, &reply);
        }
        Ok(())
    }
}
