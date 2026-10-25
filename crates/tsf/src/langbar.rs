//! `GUID_LBI_INPUTMODE` 语言栏按钮:显示中/英(大写锁定显示 A),左键切模式,
//! 右键菜单打开设置。Win10/11 的输入指示器只认这个按钮,写好转换模式
//! compartment 不会自动出现中英状态。

use std::ffi::c_void;
use std::rc::Rc;

use pinyin_protocol::Request;
use windows::Win32::Foundation::{COLORREF, E_FAIL, E_INVALIDARG, E_NOINTERFACE, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateBitmap, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER,
    DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FW_BOLD, FillRect, GetDC,
    HGDIOBJ, OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::TextServices::{
    GUID_LBI_INPUTMODE, ITfLangBarItem_Impl, ITfLangBarItemButton, ITfLangBarItemButton_Impl,
    ITfLangBarItemMgr, ITfLangBarItemSink, ITfMenu, ITfSource, ITfSource_Impl, ITfThreadMgr,
    TF_LANGBARITEMINFO, TF_LBI_CLK_RIGHT, TF_LBI_STYLE_BTN_BUTTON, TfLBIClick,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyMenu, GetForegroundWindow, HICON,
    ICONINFO, MB_ICONINFORMATION, MB_OK, MF_SEPARATOR, MF_STRING, MessageBoxW, TPM_NONOTIFY,
    TPM_RETURNCMD, TrackPopupMenu,
};
use windows::core::{BOOL, BSTR, GUID, HSTRING, Interface, PCWSTR, Ref, Result, implement};

use crate::keys;
use crate::log;
use crate::service::{Shared, SharedClient, request_with};

const MENU_TOGGLE: u32 = 1;
const MENU_PUNCT: u32 = 2;
const MENU_SETTINGS: u32 = 3;
const MENU_ABOUT: u32 = 4;

/// 登记语言栏按钮;拿不到管理器时返回 `None`(不影响打字)。
pub(crate) fn add(
    shared: Rc<Shared>,
    client: SharedClient,
    thread_mgr: &ITfThreadMgr,
) -> Option<ITfLangBarItemButton> {
    let button: ITfLangBarItemButton = ModeButton { shared, client }.into();
    let manager: ITfLangBarItemMgr = thread_mgr.cast().ok()?;
    if let Err(error) = unsafe { manager.AddItem(&button) } {
        log(&format!("登记中英指示器失败: {error}"));
        return None;
    }
    Some(button)
}

pub(crate) fn remove(button: &ITfLangBarItemButton, thread_mgr: Option<&ITfThreadMgr>) {
    let Some(thread_mgr) = thread_mgr else {
        return;
    };
    if let Ok(manager) = thread_mgr.cast::<ITfLangBarItemMgr>() {
        let _ = unsafe { manager.RemoveItem(button) };
    }
}

#[implement(ITfLangBarItemButton, ITfSource)]
struct ModeButton {
    shared: Rc<Shared>,
    client: SharedClient,
}

impl ModeButton_Impl {
    fn label(&self) -> &'static str {
        if keys::caps_on() {
            "A"
        } else if self.shared.chinese.get() {
            "中"
        } else {
            "英"
        }
    }

    fn toggle(&self) {
        // 语言栏按钮不受「Shift 单击切换」开关限制。
        let Some(reply) = request_with(
            &self.client,
            &self.shared,
            Request::ToggleMode {
                session: self.shared.session,
            },
        ) else {
            return;
        };
        // 组合中切模式会先上屏首选;点语言栏时也要写进文档。
        crate::service::apply_reply(&self.shared, &self.client, &reply);
    }

    fn open_settings() {
        let Some(dir) =
            crate::module_path_file().and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        else {
            return;
        };
        let exe = dir.join("pinyin-settings.exe");
        if let Err(error) = std::process::Command::new(&exe).spawn() {
            log(&format!("启动设置失败 {}: {error}", exe.display()));
        }
    }

    fn about() {
        let text = HSTRING::from(format!(
            "掠影 {}\nGPL-3.0-or-later\n\n词典:白霜(rime-frost)",
            env!("CARGO_PKG_VERSION")
        ));
        let caption = HSTRING::from("关于掠影");
        let owner = unsafe { GetForegroundWindow() };
        unsafe {
            MessageBoxW(
                (!owner.is_invalid()).then_some(owner),
                &text,
                &caption,
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    /// 右键自己弹菜单;不依赖语言栏是否转发 `InitMenu`。
    fn popup(&self, point: POINT) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        let toggle = HSTRING::from("切换中/英");
        let punct = HSTRING::from("切换中文标点(Ctrl+.)");
        let settings = HSTRING::from("设置…");
        let about = HSTRING::from("关于掠影…");
        unsafe {
            let _ = AppendMenuW(menu, MF_STRING, MENU_TOGGLE as usize, &toggle);
            let _ = AppendMenuW(menu, MF_STRING, MENU_PUNCT as usize, &punct);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS as usize, &settings);
            let _ = AppendMenuW(menu, MF_STRING, MENU_ABOUT as usize, &about);
        }
        let command = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                point.x,
                point.y,
                None,
                GetForegroundWindow(),
                None,
            )
        };
        unsafe {
            let _ = DestroyMenu(menu);
        }
        match command.0 as u32 {
            MENU_TOGGLE => self.toggle(),
            MENU_PUNCT => {
                let _ = request_with(&self.client, &self.shared, Request::TogglePunct);
            }
            MENU_SETTINGS => Self::open_settings(),
            MENU_ABOUT => Self::about(),
            _ => {}
        }
    }
}

impl ITfLangBarItem_Impl for ModeButton_Impl {
    fn GetInfo(&self, pinfo: *mut TF_LANGBARITEMINFO) -> Result<()> {
        if pinfo.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let info = unsafe { &mut *pinfo };
        info.clsidService = crate::CLSID_IME;
        info.guidItem = GUID_LBI_INPUTMODE;
        info.dwStyle = TF_LBI_STYLE_BTN_BUTTON;
        info.ulSort = 0;
        // 结构体由系统分配,没写的字节可能是垃圾;先清零再拷描述。
        info.szDescription = [0; 32];
        let description: Vec<u16> = "掠影中英模式".encode_utf16().collect();
        let len = description.len().min(info.szDescription.len());
        info.szDescription[..len].copy_from_slice(&description[..len]);
        Ok(())
    }

    fn GetStatus(&self) -> Result<u32> {
        Ok(0)
    }

    fn Show(&self, _fshow: BOOL) -> Result<()> {
        Ok(())
    }

    fn GetTooltipString(&self) -> Result<BSTR> {
        Ok(BSTR::from("掠影(Shift 切换中/英)"))
    }
}

impl ITfLangBarItemButton_Impl for ModeButton_Impl {
    fn OnClick(&self, click: TfLBIClick, pt: &POINT, _prcarea: *const RECT) -> Result<()> {
        if click == TF_LBI_CLK_RIGHT {
            self.popup(*pt);
        } else {
            self.toggle();
        }
        Ok(())
    }

    fn InitMenu(&self, _pmenu: Ref<ITfMenu>) -> Result<()> {
        Ok(())
    }

    fn OnMenuSelect(&self, _wid: u32) -> Result<()> {
        Ok(())
    }

    fn GetIcon(&self) -> Result<HICON> {
        icon(self.label()).ok_or_else(|| windows::core::Error::from(E_FAIL))
    }

    fn GetText(&self) -> Result<BSTR> {
        Ok(BSTR::from(self.label()))
    }
}

impl ITfSource_Impl for ModeButton_Impl {
    fn AdviseSink(&self, riid: *const GUID, punk: Ref<windows::core::IUnknown>) -> Result<u32> {
        if riid.is_null() {
            return Err(E_INVALIDARG.into());
        }
        if unsafe { *riid } != ITfLangBarItemSink::IID {
            return Err(E_NOINTERFACE.into());
        }
        let sink: ITfLangBarItemSink = punk.ok()?.cast()?;
        *self.shared.langbar_sink.borrow_mut() = Some(sink);
        Ok(1)
    }

    fn UnadviseSink(&self, _cookie: u32) -> Result<()> {
        self.shared.langbar_sink.borrow_mut().take();
        Ok(())
    }
}

/// 16×16 模式图标:圆角色块上写「中 / 英 / A」。
fn icon(label: &str) -> Option<HICON> {
    let background = if label == "中" { 0x2f6fed } else { 0x70707a };
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let memory = CreateCompatibleDC(Some(screen));
        if memory.is_invalid() {
            ReleaseDC(None, screen);
            return None;
        }
        let color = CreateCompatibleBitmap(screen, 16, 16);
        let old_bitmap = SelectObject(memory, HGDIOBJ(color.0));
        let brush = CreateSolidBrush(COLORREF(background));
        let rect = RECT {
            left: 0,
            top: 0,
            right: 16,
            bottom: 16,
        };
        FillRect(memory, &rect, brush);
        SetBkMode(memory, TRANSPARENT);
        SetTextColor(memory, COLORREF(0x00ff_ffff));
        let face: Vec<u16> = "Microsoft YaHei".encode_utf16().chain([0]).collect();
        let font = CreateFontW(
            -12,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            u32::from(DEFAULT_PITCH.0),
            PCWSTR(face.as_ptr()),
        );
        let old_font = SelectObject(memory, HGDIOBJ(font.0));
        let mut text: Vec<u16> = label.encode_utf16().collect();
        let mut text_rect = rect;
        DrawTextW(
            memory,
            &mut text,
            &mut text_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(memory, old_font);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(brush.0));
        SelectObject(memory, old_bitmap);
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);

        // 1bpp 全 0 掩码 = 整个图标不透明。16 像素宽的掩码按 4 字节
        // 行对齐,16 行共 64 字节;少给会让 CreateBitmap 越界读栈。
        let mask_bits = [0u8; 64];
        let mask = CreateBitmap(16, 16, 1, 1, Some(mask_bits.as_ptr().cast::<c_void>()));
        let info = ICONINFO {
            fIcon: BOOL(1),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let result = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(color.0));
        result
    }
}
