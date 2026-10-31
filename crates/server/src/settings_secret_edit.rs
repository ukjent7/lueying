//! 扩展密钥编辑器:设置窗里的原生单行文本输入。
//!
//! 密钥不经过自绘文本框:标准 EDIT 负责输入法、选区、读屏与 UIA
//! TextPattern;保存走 `settings_secret` 的原子写,窗口只回一条刷新消息。

use std::ffi::c_void;
use std::sync::Mutex;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, DEFAULT_GUI_FONT, GetStockObject, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, SetFocus, VK_ESCAPE, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOHSCROLL, GWLP_USERDATA, GetClientRect,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IDC_ARROW, IsChild,
    LoadCursorW, MB_ICONERROR, MB_OK, MSG, MessageBoxW, MoveWindow, PostMessageW, RegisterClassW,
    SW_SHOW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_KEYDOWN,
    WM_SETFONT, WM_SIZE, WNDCLASSW, WS_CHILD, WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::settings_secret;

/// 密钥保存后通知设置窗刷新「已设置/未设置」。
pub const WM_SECRET_SAVED: u32 = WM_APP + 5;

static EDITOR: Mutex<Option<Handles>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct Handles {
    window: usize,
    edit: usize,
    save: usize,
    cancel: usize,
}

struct Editor {
    owner: HWND,
    ext: String,
    key: String,
    edit: HWND,
    save: HWND,
    cancel: HWND,
}

/// 打开密钥编辑器;同一时间只开一个。
pub fn open(owner: HWND, ext: String, key: String, title: String) -> Result<(), String> {
    if let Some(handles) = *EDITOR.lock().unwrap_or_else(|poison| poison.into_inner()) {
        let window = HWND(handles.window as *mut c_void);
        let edit = HWND(handles.edit as *mut c_void);
        unsafe {
            let _ = SetForegroundWindow(window);
            let _ = SetFocus(Some(edit));
        }
        return Ok(());
    }
    let current = settings_secret::get(&ext, &key).unwrap_or_default();
    let (editor, edit) = create(owner, ext, key, &title, &current)?;
    unsafe {
        let _ = EnableWindow(owner, false);
        ShowWindow(editor, SW_SHOW);
        let _ = SetForegroundWindow(editor);
        let _ = SetFocus(Some(edit));
    }
    Ok(())
}

/// 设置窗消息循环先问这里:Esc 关闭、Tab 在控件间循环。
pub fn handle_key(msg: &MSG) -> bool {
    let Some(handles) = *EDITOR.lock().unwrap_or_else(|poison| poison.into_inner()) else {
        return false;
    };
    if msg.message != WM_KEYDOWN {
        return false;
    }
    let window = HWND(handles.window as *mut c_void);
    let inside = msg.hwnd == window || unsafe { IsChild(window, msg.hwnd).as_bool() };
    if !inside {
        return false;
    }
    let key = msg.wParam.0 as u32;
    if key == u32::from(VK_ESCAPE.0) {
        unsafe {
            let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        return true;
    }
    if key == u32::from(VK_TAB.0) {
        let edit = HWND(handles.edit as *mut c_void);
        let save = HWND(handles.save as *mut c_void);
        let cancel = HWND(handles.cancel as *mut c_void);
        let focus = unsafe { GetFocus() };
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 };
        let next = if focus == edit {
            if shift { cancel } else { save }
        } else if focus == save {
            if shift { edit } else { cancel }
        } else if shift {
            save
        } else {
            edit
        };
        unsafe {
            let _ = SetFocus(Some(next));
        }
        return true;
    }
    false
}

fn create(
    owner: HWND,
    ext: String,
    key: String,
    title: &str,
    text: &str,
) -> Result<(HWND, HWND), String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let hinstance = HINSTANCE(hmodule.0);
        ensure_class(hinstance)?;
        let dpi = GetDpiForWindow(owner).max(96);
        let scale = dpi as f32 / 96.0;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: (460.0 * scale) as i32,
            bottom: (170.0 * scale) as i32,
        };
        let _ = AdjustWindowRectExForDpi(
            &mut rect,
            WS_OVERLAPPEDWINDOW,
            false,
            WINDOW_EX_STYLE(0),
            dpi,
        );
        let mut owner_rect = RECT::default();
        let _ = GetWindowRect(owner, &mut owner_rect);
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let x = owner_rect.left + ((owner_rect.right - owner_rect.left) - width) / 2;
        let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top) - height) / 2;
        let window_title = HSTRING::from(format!("扩展设置 · {title}"));
        let editor = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("lueying-settings-secret"),
            PCWSTR(window_title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            width,
            height,
            Some(owner),
            None,
            Some(hinstance),
            None,
        )
        .map_err(|e| format!("建密钥编辑窗失败: {e}"))?;
        let (edit, save, cancel) = match create_children(hinstance, editor, text) {
            Ok(children) => children,
            Err(error) => {
                let _ = DestroyWindow(editor);
                return Err(error);
            }
        };
        let state = Box::new(Editor {
            owner,
            ext,
            key,
            edit,
            save,
            cancel,
        });
        SetWindowLongPtrW(editor, GWLP_USERDATA, Box::into_raw(state) as isize);
        *EDITOR.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(Handles {
            window: editor.0 as usize,
            edit: edit.0 as usize,
            save: save.0 as usize,
            cancel: cancel.0 as usize,
        });
        layout_children(editor);
        Ok((editor, edit))
    }
}

fn create_children(
    hinstance: HINSTANCE,
    editor: HWND,
    text: &str,
) -> Result<(HWND, HWND, HWND), String> {
    unsafe {
        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            0,
            1,
            1,
            Some(editor),
            None,
            Some(hinstance),
            None,
        )
        .map_err(|e| format!("建密钥文本框失败: {e}"))?;
        let save = button(hinstance, editor, w!("保存"))?;
        let cancel = button(hinstance, editor, w!("取消"))?;
        let font = GetStockObject(DEFAULT_GUI_FONT);
        for control in [edit, save, cancel] {
            SendMessageW(
                control,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
        let _ = SetWindowTextW(edit, &HSTRING::from(text));
        Ok((edit, save, cancel))
    }
}

fn button(hinstance: HINSTANCE, parent: HWND, text: PCWSTR) -> Result<HWND, String> {
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            text,
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            0,
            0,
            1,
            1,
            Some(parent),
            None,
            Some(hinstance),
            None,
        )
        .map_err(|e| format!("建密钥按钮失败: {e}"))
    }
}

fn layout_children(hwnd: HWND) {
    let Some(editor) = state_of(hwnd) else {
        return;
    };
    let mut client = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut client) }.is_err() {
        return;
    }
    let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    let pad = 12.0 * scale;
    let gap = 8.0 * scale;
    let button_w = 88.0 * scale;
    let button_h = 30.0 * scale;
    let width = client.right as f32;
    let edit_w = (width - pad * 2.0).max(120.0 * scale);
    let save_x = width - pad - button_w * 2.0 - gap;
    unsafe {
        let _ = MoveWindow(
            editor.edit,
            pad as i32,
            (pad + 10.0 * scale) as i32,
            edit_w as i32,
            (30.0 * scale) as i32,
            true,
        );
        let _ = MoveWindow(
            editor.save,
            save_x as i32,
            (pad + 56.0 * scale) as i32,
            button_w as i32,
            button_h as i32,
            true,
        );
        let _ = MoveWindow(
            editor.cancel,
            (width - pad - button_w) as i32,
            (pad + 56.0 * scale) as i32,
            button_w as i32,
            button_h as i32,
            true,
        );
    }
}

fn save_and_close(hwnd: HWND) {
    let Some(editor) = state_of(hwnd) else {
        return;
    };
    let text = text_of(editor.edit);
    let ext = editor.ext.clone();
    let key = editor.key.clone();
    let owner = editor.owner;
    if let Err(error) = settings_secret::set(&ext, &key, &text) {
        unsafe {
            let _ = MessageBoxW(
                Some(hwnd),
                &HSTRING::from(format!("保存密钥失败: {error}")),
                w!("扩展设置"),
                MB_OK | MB_ICONERROR,
            );
        }
        return;
    }
    unsafe {
        let _ = PostMessageW(Some(owner), WM_SECRET_SAVED, WPARAM(0), LPARAM(0));
        let _ = DestroyWindow(hwnd);
    }
}

fn text_of(edit: HWND) -> String {
    let length = unsafe { GetWindowTextLengthW(edit) };
    if length <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let read = unsafe { GetWindowTextW(edit, &mut buffer) };
    String::from_utf16_lossy(&buffer[..read.max(0) as usize])
}

fn state_of(hwnd: HWND) -> Option<&'static mut Editor> {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Editor;
        ptr.as_mut()
    }
}

fn finish(hwnd: HWND) {
    let owner = state_of(hwnd).map(|editor| editor.owner);
    if let Some(owner) = owner {
        unsafe {
            let _ = EnableWindow(owner, true);
            let _ = SetForegroundWindow(owner);
        }
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if ptr != 0 {
        unsafe {
            drop(Box::from_raw(ptr as *mut Editor));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
    }
    *EDITOR.lock().unwrap_or_else(|poison| poison.into_inner()) = None;
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                layout_children(hwnd);
                LRESULT(0)
            }
            WM_COMMAND => {
                let target = HWND(lparam.0 as *mut c_void);
                let buttons = state_of(hwnd).map(|editor| (editor.save, editor.cancel));
                if let Some((save, cancel)) = buttons {
                    if target == save {
                        save_and_close(hwnd);
                    } else if target == cancel {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                finish(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn ensure_class(hinstance: HINSTANCE) -> Result<(), String> {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: w!("lueying-settings-secret"),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 as isize + 1) as *mut c_void),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW(密钥编辑窗) 失败".into());
        }
    });
    result
}
