//! 截图提供者:GDI 位块传输编 BMP,落临时目录 `lueying-capture`。

use std::path::PathBuf;

use lueying_extwasm::{Done, Provider};

use serde_json::json;

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPFILEHEADER, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY,
    SelectObject,
};

use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::one_method;

/// 截屏;`region` 缺省是整个虚拟桌面。在调用线程上执行 GDI 全程。
pub struct CaptureProvider;

impl Provider for CaptureProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        one_method("capture", method, "shot")?;
        let region = parse_region(args)?;
        std::thread::Builder::new()
            .name("ext-capture".into())
            .spawn(move || {
                let result = shot_bmp(region).and_then(|(file, width, height)| {
                    serde_json::to_string(&json!({
                        "file": file.to_string_lossy(),
                        "width": width,
                        "height": height,
                    }))
                    .map_err(|e| e.to_string())
                });
                done.complete(result);
            })
            .map_err(|e| format!("截图线程启动失败: {e}"))?;
        Ok(())
    }
}

/// 截图区域;虚拟桌面坐标。
pub(super) struct Region {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

/// `{"region":{"x":..,"y":..,"width":..,"height":..}}` 或缺省整屏。
pub(super) fn parse_region(args: &str) -> Result<Option<Region>, String> {
    if args.is_empty() {
        return Ok(None);
    }
    let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
    let Some(region) = parsed.get("region") else {
        return Ok(None);
    };
    if region.is_null() {
        return Ok(None);
    }
    let field = |name: &str| -> Result<i32, String> {
        region[name]
            .as_i64()
            .map(|value| value.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
            .ok_or_else(|| format!("region.{name} 缺失或不是整数"))
    };
    Ok(Some(Region {
        x: field("x")?,
        y: field("y")?,
        width: field("width")?,
        height: field("height")?,
    }))
}

/// GDI 截一块屏幕写 32 位 BMP;返回 (文件, 宽, 高)。
fn shot_bmp(region: Option<Region>) -> Result<(PathBuf, i32, i32), String> {
    let (x, y, width, height) = match region {
        Some(region) => (region.x, region.y, region.width, region.height),
        None => unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        },
    };
    if width <= 0 || height <= 0 {
        return Err(format!("截图区域为空({width}x{height})"));
    }
    unsafe {
        let screen = GetDC(None);
        let memory = CreateCompatibleDC(Some(screen));
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width;
        bmi.bmiHeader.biHeight = height;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let section = CreateDIBSection(Some(memory), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| format!("CreateDIBSection: {e}"))?;
        let previous = SelectObject(memory, section.into());
        let _ = BitBlt(memory, 0, 0, width, height, Some(screen), x, y, SRCCOPY);
        let stride = width as usize * 4;
        let pixels =
            std::slice::from_raw_parts(bits as *const u8, stride * height as usize).to_vec();
        let _ = SelectObject(memory, previous);
        let _ = DeleteObject(section.into());
        let _ = DeleteDC(memory);
        let _ = ReleaseDC(None, screen);

        let mut file_header = BITMAPFILEHEADER::default();
        file_header.bfType = 0x4d42;
        file_header.bfSize = (54 + pixels.len()) as u32;
        file_header.bfOffBits = 54;
        let header_bytes = std::slice::from_raw_parts(
            (&file_header as *const BITMAPFILEHEADER) as *const u8,
            std::mem::size_of::<BITMAPFILEHEADER>(),
        );
        let info_bytes = std::slice::from_raw_parts(
            (&BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            } as *const BITMAPINFOHEADER) as *const u8,
            std::mem::size_of::<BITMAPINFOHEADER>(),
        );
        let dir = std::env::temp_dir().join("lueying-capture");
        std::fs::create_dir_all(&dir).map_err(|e| format!("建截图目录失败: {e}"))?;
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("shot-{millis}-{}.bmp", std::process::id()));
        let mut bytes = Vec::with_capacity(pixels.len() + 54);
        bytes.extend_from_slice(header_bytes);
        bytes.extend_from_slice(info_bytes);
        bytes.extend_from_slice(&pixels);
        std::fs::write(&path, bytes).map_err(|e| format!("写截图失败: {e}"))?;
        Ok((path, width, height))
    }
}
