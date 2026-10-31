//! 设置窗的顶部搜索框:原生单行 EDIT 的创建、布局与键盘循环。
//!
//! 子模块挂在 `settings_win` 下,和窗口共用私有字段;搜索词变化只改
//! `Settings::search` 并重排,绘制仍由 `lueying_render::ui` 负责。

use super::*;
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, GetStockObject, HGDIOBJ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_DOWN, VK_ESCAPE, VK_TAB};
use windows::Win32::UI::WindowsAndMessaging::{
    ES_AUTOHSCROLL, GetWindowTextLengthW, GetWindowTextW, WM_SETFONT,
};

/// 搜索框里的键盘循环:Esc 清空或关闭,Tab/↓ 把焦点交给设置页。
pub(super) fn handle_search_key(hwnd: HWND, msg: &MSG) -> bool {
    if msg.message != WM_KEYDOWN {
        return false;
    }
    let Some(window) = state_of(hwnd) else {
        return false;
    };
    let Some(edit) = window.search_edit else {
        return false;
    };
    if msg.hwnd != edit {
        return false;
    }
    let key = msg.wParam.0 as u32;
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
    match key {
        k if k == u32::from(VK_ESCAPE.0) => {
            if window.settings.search.is_empty() {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            } else {
                window.settings.set_search("");
                window.sync_search_text();
                window.relayout();
            }
            true
        }
        k if k == u32::from(VK_TAB.0) || k == u32::from(VK_DOWN.0) => {
            unsafe {
                let _ = SetFocus(Some(hwnd));
            }
            window.focus_step(if shift && k == u32::from(VK_TAB.0) {
                -1
            } else {
                1
            });
            true
        }
        _ => false,
    }
}

impl Window {
    /// 建顶部搜索框:标准 EDIT,输入法、选区、读屏与 UIA TextPattern 由系统负责。
    pub(super) fn build_search_edit(&mut self, parent: HWND) -> Result<(), String> {
        unsafe {
            let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                windows::core::PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                0,
                0,
                1,
                1,
                Some(parent),
                None,
                Some(HINSTANCE(hmodule.0)),
                None,
            )
            .map_err(|e| format!("建搜索框失败: {e}"))?;
            let font = GetStockObject(DEFAULT_GUI_FONT);
            SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            let cue = HSTRING::from("搜索设置");
            SendMessageW(
                edit,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(cue.as_ptr() as isize)),
            );
            self.search_edit = Some(edit);
            self.sync_search_text();
            self.layout_search();
        }
        Ok(())
    }

    /// 搜索框背景画刷跟随主题;旧画刷先删掉。
    pub(super) fn refresh_search_brush(&mut self) {
        if let Some(old) = self.search_brush.take() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(old.0));
            }
        }
        self.search_brush = Some(unsafe { CreateSolidBrush(COLORREF(self.palette.surface_alt)) });
    }

    /// 把原生 EDIT 放到页面几何给出的搜索框矩形里。
    pub(super) fn layout_search(&self) {
        let Some(edit) = self.search_edit else {
            return;
        };
        let rect = self.layout.search;
        let inset = 2.0 * self.scale;
        unsafe {
            let _ = MoveWindow(
                edit,
                (rect.left + inset) as i32,
                (rect.top + inset) as i32,
                (rect.width() - inset * 2.0).max(20.0) as i32,
                (rect.height() - inset * 2.0).max(16.0) as i32,
                true,
            );
        }
    }

    /// 程序改搜索词后同步到 EDIT;用户输入触发的 EN_CHANGE 不打断光标。
    pub(super) fn sync_search_text(&mut self) {
        let Some(edit) = self.search_edit else {
            return;
        };
        if self.syncing_search {
            return;
        }
        let current = read_edit_text(edit);
        if current == self.settings.search {
            return;
        }
        self.syncing_search = true;
        unsafe {
            let _ = SetWindowTextW(edit, &HSTRING::from(self.settings.search.as_str()));
        }
        self.syncing_search = false;
    }

    /// 用户改了搜索词:更新页面并重排。
    pub(super) fn search_changed(&mut self) {
        if self.syncing_search {
            return;
        }
        let Some(edit) = self.search_edit else {
            return;
        };
        let text = read_edit_text(edit);
        if text == self.settings.search {
            return;
        }
        self.settings.set_search(text);
        self.relayout();
    }
}

/// 读标准 EDIT 的全部文字。
pub(super) fn read_edit_text(edit: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(edit) };
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let read = unsafe { GetWindowTextW(edit, &mut buffer) };
    String::from_utf16_lossy(&buffer[..read.max(0) as usize])
}
