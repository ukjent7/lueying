//! 输入法面板的 Direct2D 绘制表面:画到 32 位预乘位图,再上屏。
//!
//! 徽标走分层窗口(`UpdateLayeredWindow`),命令面板与选择面板是不透明
//! 窗口(`BitBlt`)。三条路共用同一块 DIB 与同一套 Direct2D/DirectWrite
//! 绘制代码;候选窗的 GPU 路径在 `ui::d2d::pipe`,不经过这里。

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::ptr;

use lueying_render::paint::{Canvas, Color, Point};
use lueying_render::panel::{Metrics, Rect};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BitBlt, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY,
    SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::{HSTRING, Interface, w};

/// `GENERIC_READ`:以只读方式打开图片文件。
const GENERIC_READ_ACCESS: u32 = 0x8000_0000;

/// 不限制宽度时给文字布局的可用宽度(物理像素);D2D 不接受无穷大。
const TEXT_LIMIT: f32 = 4096.0;

/// 文字对齐方式;同字号的不同对齐各缓存一份,格式对象只读不改。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Align {
    /// 左对齐、行盒竖直居中。
    Leading,
    /// 水平与竖直都居中。
    Centered,
}

/// 一层可绘制的位图表面;窗口线程独占。
pub struct Surface {
    dwrite: IDWriteFactory,
    /// 绑到 `mem_dc` 的绘制目标;删 GDI 资源前先置空。
    target: Option<ID2D1DCRenderTarget>,
    brush: ID2D1SolidColorBrush,
    mem_dc: HDC,
    dib: Option<HBITMAP>,
    old: Option<HGDIOBJ>,
    size: (u32, u32),
    formats: HashMap<(u32, Align), IDWriteTextFormat>,
    drawing: bool,
    /// 动画帧位图,下标即帧号。
    sprites: Vec<ID2D1Bitmap>,
}

impl Surface {
    /// 建一块内存 DC 与 Direct2D 工厂;没有桌面会话时返回错误。
    pub fn new() -> Result<Self, String> {
        unsafe {
            let screen = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen));
            let _ = ReleaseDC(None, screen);
            if mem_dc.is_invalid() {
                return Err("CreateCompatibleDC 失败".into());
            }
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| format!("D2D1CreateFactory: {e}"))?;
            let properties = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                // 画布坐标全是物理像素:按 96 DPI 解释,1 DIP = 1 像素。
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory
                .CreateDCRenderTarget(&properties)
                .map_err(|e| format!("CreateDCRenderTarget: {e}"))?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let brush = target
                .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
                .map_err(|e| format!("CreateSolidColorBrush: {e}"))?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
                .map_err(|e| format!("DWriteCreateFactory: {e}"))?;
            Ok(Self {
                dwrite,
                target: Some(target),
                brush,
                mem_dc,
                dib: None,
                old: None,
                size: (0, 0),
                formats: HashMap::new(),
                drawing: false,
                sprites: Vec::new(),
            })
        }
    }

    /// 开始一帧:按需重建位图并清成透明。返回的画布尺寸是物理像素。
    pub fn begin(&mut self, w: u32, h: u32) -> Result<&mut Self, String> {
        self.end();
        self.ensure(w, h)?;
        let target = self.target()?;
        unsafe {
            target.BeginDraw();
            // 空颜色指针就是透明黑,与 D2D1_COLOR_F::default() 等价。
            target.Clear(None);
        }
        self.drawing = true;
        Ok(self)
    }

    /// 不透明窗口:EndDraw 后从内存 DC 贴到窗口客户区。
    pub fn blit(&mut self, hdc: HDC, w: i32, h: i32) -> Result<(), String> {
        self.end();
        unsafe {
            BitBlt(hdc, 0, 0, w, h, Some(self.mem_dc), 0, 0, SRCCOPY)
                .map_err(|e| format!("BitBlt: {e}"))
        }
    }

    /// 分层窗口:整幅贴到窗口的指定位置,alpha 参与混合。
    pub fn present(&mut self, hwnd: HWND, x: i32, y: i32, w: i32, h: i32) -> Result<(), String> {
        self.end();
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
                Some(&dest as *const POINT),
                Some(&size as *const SIZE),
                Some(self.mem_dc),
                None,
                COLORREF(0),
                Some(&blend as *const BLENDFUNCTION),
                ULW_ALPHA,
            )
            .map_err(|e| format!("UpdateLayeredWindow: {e}"))
        }
    }

    /// 借出绘制目标的克隆;COM 引用计数加一,调用方拿得住,也不挡住画刷的可变借用。
    /// 换一组动画帧,下标即帧号。
    pub fn set_sprites(&mut self, sprites: Vec<ID2D1Bitmap>) {
        self.sprites = sprites;
    }

    /// 用 WIC 解码一张图片的第一帧,转成这块表面能画的位图。
    pub fn load_sprite(&self, path: &Path) -> Result<ID2D1Bitmap, String> {
        let target = self.target()?;
        unsafe {
            // 窗口线程已经初始化过 COM 时这里返回 RPC_E_CHANGED_MODE,无影响。
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| format!("WIC 工厂: {e}"))?;
            let decoder = factory
                .CreateDecoderFromFilename(
                    &HSTRING::from(path.as_os_str()),
                    None,
                    GENERIC_READ_ACCESS,
                    WICDecodeMetadataCacheOnDemand,
                )
                .map_err(|e| format!("打开 {}: {e}", path.display()))?;
            let frame = decoder.GetFrame(0).map_err(|e| format!("读第一帧: {e}"))?;
            let frame_source: IWICBitmapSource =
                frame.cast().map_err(|e| format!("帧接口: {e}"))?;
            let converter = factory
                .CreateFormatConverter()
                .map_err(|e| format!("格式转换器: {e}"))?;
            converter
                .Initialize(
                    &frame_source,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None::<&IWICPalette>,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .map_err(|e| format!("转 BGRA: {e}"))?;
            let source: IWICBitmapSource = converter.cast().map_err(|e| format!("转接口: {e}"))?;
            target
                .CreateBitmapFromWicBitmap(&source, None)
                .map_err(|e| format!("建位图: {e}"))
        }
    }

    fn target(&self) -> Result<ID2D1DCRenderTarget, String> {
        self.target
            .clone()
            .ok_or_else(|| "绘制目标已释放".to_string())
    }

    fn end(&mut self) {
        if self.drawing {
            if let Some(target) = self.target.as_ref() {
                let _ = unsafe { target.EndDraw(None, None) };
            }
            self.drawing = false;
        }
    }

    /// 尺寸变了就重建位图并重新绑定绘制目标。
    fn ensure(&mut self, w: u32, h: u32) -> Result<(), String> {
        if self.size == (w, h) {
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
                    // 负高度:位图自上而下,和 Direct2D 的行序一致。
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
            self.old = Some(old);
            self.size = (w, h);
            let rect = RECT {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            };
            self.target()?
                .BindDC(self.mem_dc, &rect)
                .map_err(|e| format!("BindDC: {e}"))?;
        }
        Ok(())
    }

    fn format(&mut self, size: f32, align: Align) -> Option<IDWriteTextFormat> {
        let key = (size.to_bits(), align);
        if let Some(format) = self.formats.get(&key) {
            return Some(format.clone());
        }
        let format = unsafe {
            self.dwrite
                .CreateTextFormat(
                    w!("Microsoft YaHei UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("zh-CN"),
                )
                .ok()?
        };
        let text = match align {
            Align::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Centered => DWRITE_TEXT_ALIGNMENT_CENTER,
        };
        unsafe {
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP).ok()?;
            format
                .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
                .ok()?;
            format.SetTextAlignment(text).ok()?;
        }
        self.formats.insert(key, format.clone());
        Some(format)
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.end();
        // 先释放绘制目标,再删它绑定的 DC。
        self.target = None;
        unsafe {
            if let (Some(dib), Some(old)) = (self.dib, self.old) {
                SelectObject(self.mem_dc, old);
                let _ = DeleteObject(HGDIOBJ(dib.0));
            }
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

fn d2d_color(color: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((color.rgb >> 16) & 0xff) as f32 / 255.0,
        g: ((color.rgb >> 8) & 0xff) as f32 / 255.0,
        b: (color.rgb & 0xff) as f32 / 255.0,
        a: f32::from(color.alpha) / 255.0,
    }
}

fn rect(rect: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

fn rrect(rect: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: self::rect(rect),
        radiusX: radius,
        radiusY: radius,
    }
}

impl Canvas for Surface {
    fn fill_rect(&mut self, rect: Rect, color: Color) {
        let Ok(target) = self.target() else {
            return;
        };
        unsafe {
            self.brush.SetColor(&d2d_color(color));
            target.FillRectangle(&self::rect(rect), &self.brush);
        }
    }

    fn draw_sprite(&mut self, sprite: usize, rect: Rect) {
        let Ok(target) = self.target() else {
            return;
        };
        let Some(bitmap) = self.sprites.get(sprite) else {
            return;
        };
        unsafe {
            target.DrawBitmap(
                bitmap,
                Some(&self::rect(rect) as *const D2D_RECT_F),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
    }

    fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color) {
        let Ok(target) = self.target() else {
            return;
        };
        unsafe {
            self.brush.SetColor(&d2d_color(color));
            target.DrawRectangle(
                &self::rect(rect),
                &self.brush,
                width,
                None::<&ID2D1StrokeStyle>,
            );
        }
    }

    fn fill_rrect(&mut self, rect: Rect, radius: f32, color: Color) {
        let Ok(target) = self.target() else {
            return;
        };
        unsafe {
            self.brush.SetColor(&d2d_color(color));
            target.FillRoundedRectangle(&rrect(rect, radius), &self.brush);
        }
    }

    fn stroke_rrect(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        let Ok(target) = self.target() else {
            return;
        };
        unsafe {
            self.brush.SetColor(&d2d_color(color));
            target.DrawRoundedRectangle(
                &rrect(rect, radius),
                &self.brush,
                width,
                None::<&ID2D1StrokeStyle>,
            );
        }
    }

    fn shadow_rrect(&mut self, rect: Rect, radius: f32, blur: f32, color: Color) {
        // 位图表面拿不到 Direct2D 特效图;向外叠几层淡圆角矩形近似高斯阴影。
        if color.alpha == 0 {
            return;
        }
        const STEPS: u8 = 5;
        let step_alpha = (color.alpha / (STEPS + 1) / 2).max(1);
        for step in (1..=STEPS).rev() {
            let grow = blur * f32::from(step) / f32::from(STEPS);
            let outer = Rect::from_ltrb(
                rect.left - grow,
                rect.top - grow,
                rect.right + grow,
                rect.bottom + grow,
            );
            self.fill_rrect(
                outer,
                radius + grow,
                Color::with_alpha(color.rgb, step_alpha),
            );
        }
    }

    fn text(
        &mut self,
        content: &str,
        origin: Point,
        size: f32,
        color: Color,
        max_width: Option<f32>,
    ) {
        let Some(format) = self.format(size, Align::Leading) else {
            return;
        };
        let Ok(target) = self.target() else {
            return;
        };
        // 锚点是左缘与竖直中线:行盒上下各留一个字高,段落居中后字就在中线上。
        let area = D2D_RECT_F {
            left: origin.x,
            top: origin.y - size,
            right: origin.x + max_width.unwrap_or(TEXT_LIMIT),
            bottom: origin.y + size,
        };
        // 限宽时按绘制矩形裁边,溢出部分不会画到别的控件上。
        let clip = max_width.is_some();
        unsafe {
            self.brush.SetColor(&d2d_color(color));
            let utf16: Vec<u16> = content.encode_utf16().collect();
            if clip {
                target.PushAxisAlignedClip(&area, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            target.DrawText(
                &utf16,
                &format,
                &area,
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            if clip {
                target.PopAxisAlignedClip();
            }
        }
    }

    fn measure(&mut self, content: &str, size: f32) -> f32 {
        let Some(format) = self.format(size, Align::Leading) else {
            return size * Metrics::text_units(content);
        };
        let utf16: Vec<u16> = content.encode_utf16().collect();
        let layout = unsafe {
            self.dwrite
                .CreateTextLayout(&utf16, &format, TEXT_LIMIT, TEXT_LIMIT)
        };
        let Ok(layout) = layout else {
            return size * Metrics::text_units(content);
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        if unsafe { layout.GetMetrics(&mut metrics) }.is_err() {
            return size * Metrics::text_units(content);
        }
        metrics.widthIncludingTrailingWhitespace
    }

    fn text_centered(&mut self, content: &str, rect: Rect, size: f32, color: Color) {
        let Some(format) = self.format(size, Align::Centered) else {
            return;
        };
        let Ok(target) = self.target() else {
            return;
        };
        unsafe {
            self.brush.SetColor(&d2d_color(color));
            let utf16: Vec<u16> = content.encode_utf16().collect();
            target.DrawText(
                &utf16,
                &format,
                &self::rect(rect),
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
}
