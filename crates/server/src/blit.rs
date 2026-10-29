//! 光栅表面与屏幕之间的搬运:Skia 画到一块 32 位预乘位图,再上屏。
//!
//! 候选窗走 DirectComposition,徽标走分层窗口,设置窗是不透明窗口——
//! 三条路共用的只有「像素怎么进 GDI 位图」这一步,放在这里。

use std::ffi::c_void;
use std::ptr;

use skia_safe::Surface;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY,
    SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};

/// 整幅像素读回(N32 预乘;小端机上字节序是 BGRA)。
pub fn read_pixels(surface: &mut Surface) -> Option<Vec<u8>> {
    let (w, h) = (surface.width(), surface.height());
    let info = skia_safe::ImageInfo::new_n32_premul((w, h), None);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    surface
        .read_pixels(&info, &mut buf, info.min_row_bytes(), (0, 0))
        .then_some(buf)
}

/// 光栅表面 + GDI 位图:Skia 画完把像素搬进位图,再上屏。
pub struct Layer {
    surface: Option<Surface>,
    mem_dc: HDC,
    dib: Option<HBITMAP>,
    /// 位图像素(32 位 BGRA 预乘)。
    bits: *mut c_void,
    /// 换回旧位图才敢删 DIB。
    old: Option<HGDIOBJ>,
    size: (u32, u32),
}

impl Layer {
    /// 建一块内存 DC;没有桌面会话时返回错误。
    pub fn new() -> Result<Self, String> {
        unsafe {
            let screen = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen));
            let _ = ReleaseDC(None, screen);
            if mem_dc.is_invalid() {
                return Err("CreateCompatibleDC 失败".into());
            }
            Ok(Self {
                surface: None,
                mem_dc,
                dib: None,
                bits: ptr::null_mut(),
                old: None,
                size: (0, 0),
            })
        }
    }

    /// 取表面;尺寸变了就重建表面与位图。
    pub fn surface(&mut self, w: u32, h: u32) -> Result<&mut Surface, String> {
        self.ensure(w, h)?;
        self.surface
            .as_mut()
            .ok_or_else(|| "没有光栅表面".to_string())
    }

    /// 把表面像素搬进位图。
    pub fn copy_pixels(&mut self) -> Result<(), String> {
        if self.bits.is_null() {
            return Err("位图还没建好".into());
        }
        let Some(surface) = self.surface.as_mut() else {
            return Err("没有光栅表面".into());
        };
        let pixels = read_pixels(surface).ok_or("像素读回失败")?;
        unsafe {
            if !pixels.is_empty() {
                ptr::copy_nonoverlapping(pixels.as_ptr(), self.bits.cast::<u8>(), pixels.len());
            }
        }
        Ok(())
    }

    /// 不透明窗口:从内存 DC 贴到窗口客户区。
    pub fn blit(&self, hdc: HDC, w: i32, h: i32) -> Result<(), String> {
        unsafe {
            windows::Win32::Graphics::Gdi::BitBlt(hdc, 0, 0, w, h, Some(self.mem_dc), 0, 0, SRCCOPY)
                .map_err(|e| format!("BitBlt: {e}"))
        }
    }

    /// 分层窗口:整幅贴到窗口的指定位置,alpha 参与混合。
    pub fn present(&mut self, hwnd: HWND, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
        self.copy_pixels()?;
        unsafe {
            let dest = POINT { x, y };
            let size = SIZE { cx: w, cy: h };
            // AC_SRC_OVER + AC_SRC_ALPHA:按源位图的 alpha 混合。
            let blend = BLENDFUNCTION {
                BlendOp: 0,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: 1,
            };
            UpdateLayeredWindow(
                hwnd,
                None,
                Some(&dest),
                Some(&size),
                Some(self.mem_dc),
                None,
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .map_err(|e| format!("UpdateLayeredWindow: {e}"))
        }
    }

    /// 尺寸变了就重建表面与位图。
    fn ensure(&mut self, w: u32, h: u32) -> Result<(), String> {
        if self.size == (w, h) && self.surface.is_some() {
            return Ok(());
        }
        unsafe {
            // 旧位图先换回去再删,GDI 里不留野句柄。
            if let (Some(dib), Some(old)) = (self.dib, self.old) {
                SelectObject(self.mem_dc, old);
                let _ = DeleteObject(HGDIOBJ(dib.0));
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    // 负高度:位图自上而下,和 Skia 的行序一致。
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = ptr::null_mut();
            let dib =
                CreateDIBSection(Some(self.mem_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                    .map_err(|e| format!("CreateDIBSection: {e}"))?;
            let old = SelectObject(self.mem_dc, HGDIOBJ(dib.0));
            self.dib = Some(dib);
            self.bits = bits;
            self.old = Some(old);
        }
        self.surface = skia_safe::surfaces::raster_n32_premul((w as i32, h as i32));
        self.size = (w, h);
        Ok(())
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        unsafe {
            if let (Some(dib), Some(old)) = (self.dib, self.old) {
                SelectObject(self.mem_dc, old);
                let _ = DeleteObject(HGDIOBJ(dib.0));
            }
            let _ = DeleteDC(self.mem_dc);
        }
    }
}
