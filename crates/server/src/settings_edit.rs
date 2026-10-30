//! 短语编辑器:设置窗里的原生文本输入。
//!
//! AccessKit 只给无障碍树,不给文本控件;短语要能改,就得有一个真的
//! EDIT。这里用标准 Win32 控件建一个小窗:EDIT 自己处理输入法、选区、
//! 读屏与 UIA TextPattern,宿主只负责存盘、Tab 循环与 Esc 关闭。

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, DEFAULT_GUI_FONT, HBRUSH, GetStockObject};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, SetFocus, VK_ESCAPE, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOVSCROLL, ES_MULTILINE, ES_WANTRETURN,
    GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, IDC_ARROW, IsChild, LoadCursorW, MB_ICONERROR, MB_OK, MSG, MessageBoxW,
    MoveWindow, PostMessageW, RegisterClassW, SW_SHOW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_KEYDOWN, WM_SETFONT, WM_SIZE, WNDCLASSW, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::config;
use crate::settings_actions;

/// 短语存盘后通知设置窗刷新提示。
pub const WM_PHRASES_SAVED: u32 = WM_APP + 3;

/// 窗内编辑器的句柄;同一时间只开一个。
static EDITOR: Mutex<Option<Handles>> = Mutex::new(None);

#[derive(Clone, Copy)]
struct Handles {
    window: usize,
    edit: usize,
    save: usize,
    cancel: usize,
}

/// 编辑器状态挂在窗口的 USERDATA 上。
struct Editor {
    owner: HWND,
    path: PathBuf,
    edit: HWND,
    save: HWND,
    cancel: HWND,
}

/// 打开短语编辑器;已经开着就把它提到前面。
pub fn open(owner: HWND) -> Result<(), String> {
    if let Some(handles) = *EDITOR.lock().unwrap_or_else(|poison| poison.into_inner()) {
        let window = HWND(handles.window as *mut c_void);
        let edit = HWND(handles.edit as *mut c_void);
        unsafe {
            let _ = SetForegroundWindow(window);
            let _ = SetFocus(Some(edit));
        }
        return Ok(());
    }
    let path = config::phrases_path().ok_or("没有可写的短语文件路径")?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if !path.exists() {
        std::fs::write(&path, lueying_features::phrases::TEMPLATE)
            .map_err(|e| format!("写短语模板失败: {e}"))?;
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读短语文件失败: {e}"))?;
    let (editor, edit) = create(owner, path, &text)?;
    unsafe {
        let _ = EnableWindow(owner, false);
        ShowWindow(editor, SW_SHOW);
        let _ = SetForegroundWindow(editor);
        let _ = SetFocus(Some(edit));
    }
    Ok(())
}

/// 设置窗的消息循环先问这里:Esc 关闭、Tab 在控件间循环。
///
/// 标准控件不会自己组成对话框的键盘循环,补上这两条,读屏用户与键盘
/// 用户就不用摸鼠标。
pub fn handle_key(msg: &MSG) -> bool {
    let handles = *EDITOR.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(handles) = handles else {
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

fn create(owner: HWND, path: PathBuf, text: &str) -> Result<(HWND, HWND), String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let hinstance = HINSTANCE(hmodule.0);
        ensure_class(hinstance)?;
        let dpi = GetDpiForWindow(owner).max(96);
        let scale = dpi as f32 / 96.0;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: (560.0 * scale) as i32,
            bottom: (420.0 * scale) as i32,
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
        let editor = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("lueying-settings-edit"),
            w!("编辑自定义短语"),
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
        .map_err(|e| format!("建短语编辑窗失败: {e}"))?;
        let result = create_children(hinstance, editor, text);
        let (edit, save, cancel) = match result {
            Ok(children) => children,
            Err(error) => {
                let _ = DestroyWindow(editor);
                return Err(error);
            }
        };
        let state = Box::new(Editor {
            owner,
            path,
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
            windows::core::PCWSTR::null(),
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | WS_VSCROLL
                | WINDOW_STYLE(ES_MULTILINE as u32)
                | WINDOW_STYLE(ES_AUTOVSCROLL as u32)
                | WINDOW_STYLE(ES_WANTRETURN as u32),
            0,
            0,
            1,
            1,
            Some(editor),
            None,
            Some(hinstance),
            None,
        )
        .map_err(|e| format!("建短语文本框失败: {e}"))?;
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
        .map_err(|e| format!("建短语按钮失败: {e}"))
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
    let height = client.bottom as f32;
    let edit_w = (width - pad * 2.0).max(120.0 * scale);
    let edit_h = (height - pad * 3.0 - button_h).max(60.0 * scale);
    let save_x = width - pad - button_w * 2.0 - gap;
    let buttons_y = pad + edit_h + gap;
    unsafe {
        let _ = MoveWindow(
            editor.edit,
            pad as i32,
            pad as i32,
            edit_w as i32,
            edit_h as i32,
            true,
        );
        let _ = MoveWindow(
            editor.save,
            save_x as i32,
            buttons_y as i32,
            button_w as i32,
            button_h as i32,
            true,
        );
        let _ = MoveWindow(
            editor.cancel,
            (width - pad - button_w) as i32,
            buttons_y as i32,
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
    let path = editor.path.clone();
    let owner = editor.owner;
    if let Err(e) = std::fs::write(&path, text) {
        unsafe {
            let _ = MessageBoxW(
                Some(hwnd),
                &HSTRING::from(format!("写入 {} 失败: {e}", path.display())),
                w!("保存短语"),
                MB_OK | MB_ICONERROR,
            );
        }
        return;
    }
    settings_actions::reload();
    unsafe {
        let _ = PostMessageW(Some(owner), WM_PHRASES_SAVED, WPARAM(0), LPARAM(0));
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
            lpszClassName: w!("lueying-settings-edit"),
            hbrBackground: HBRUSH((COLOR_WINDOW.0 as isize + 1) as *mut c_void),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW(短语编辑窗) 失败".into());
        }
    });
    result
}
