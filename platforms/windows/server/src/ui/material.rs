//! 候选窗与工具栏共用的材质:屏幕采样、连续圆角裁切与真实环境釉光。
//!
//! 玻璃的桌面像素经 BitBlt 原样抓进窗口(抓屏薄层),折射/色散/
//! frost 全部在 rune-fx lens 的 GPU 着色器里。这里只放纯采样与
//! 共用渲染构件,窗口摆位、节流与生命周期由各自 overlay 管理。

use lueying_render::panel::Palette;
use lueying_server::theme::{self, Glaze};
use rune::{El, OverlayHandle, OverlaySupport, Pixels, el};

/// 窗口的捕获排除是否已足以安全 BitBlt:Requested 与 Supported 都
/// 表示系统已受理或平台本身支持;Unknown(窗口尚未建好)、Failed 与
/// Unsupported 一律不采,宁可退实底也不把自身录进背板。
pub(super) fn desktop_capture_ready<T: 'static>(handle: Option<&OverlayHandle<T>>) -> bool {
    handle.is_some_and(|handle| {
        matches!(
            handle.capture_exclusion(),
            OverlaySupport::Requested | OverlaySupport::Supported
        )
    })
}

/// 更新一路 RGBA 像素流(背板或环境棱缘)。同尺寸已挂载时只
/// `Pixels::set` 原地流式换像素,不需要重跑 view,故返回 false;首次
/// 挂载或尺寸变化仍要重建,返回 true。
pub(super) fn set_stream(slot: &mut Option<Pixels>, size: (u32, u32), rgba: Vec<u8>) -> bool {
    let mounted = slot.as_ref().is_some_and(|pixels| pixels.size() == size);
    slot.get_or_insert_with(Pixels::new)
        .set(size.0, size.1, rgba);
    !mounted
}

/// 面板外壳与玻璃背板共用同一个连续圆角与裁切。
pub(super) fn rounded_surface(el: El, radius: f32) -> El {
    el.radius(radius).continuous_corners().clip_rounded(radius)
}

/// 黑瓷面:不透的共享石墨胎体配结构描边;釉光只来自真实环境采样,
/// 采不到保持哑光。`height` 是面板逻辑高度,决定顶部光泽的渐隐范围。
pub(super) fn ceramic_face(
    mut face: El,
    palette: &Palette,
    glaze: Option<&Glaze>,
    stroke: f32,
    height: f32,
) -> El {
    face = face
        .fill(theme::srgb(palette.bg))
        .border(1.0 * stroke, theme::srgb(palette.border));
    if let Some(glaze) = glaze {
        face = face
            .child(
                el().pin(Some(0.0), Some(0.0), Some(0.0), None)
                    .h(height * 0.45)
                    .gradient(glaze.sheen_top, glaze.sheen_top.alpha(0.0)),
            )
            .child(
                el().pin(Some(0.0), Some(0.0), Some(0.0), None)
                    .h(1.0 * stroke)
                    .fill(glaze.edge_top),
            )
            .child(
                el().pin(Some(0.0), None, Some(0.0), Some(0.0))
                    .h(1.0 * stroke)
                    .fill(glaze.edge_bottom),
            );
    }
    face
}

/// 采样面板上下两条窄带的桌面像素均值,算出釉光的环境色。两侧都有
/// 采样时各取各的转换;只有一侧时,缺失侧用透明覆盖,绝不把一侧的
/// 光镜像到没有来源的另一侧。两条都采不到就返回 None(哑光)。
pub(super) fn sample_glaze(rect: (i32, i32, i32, i32)) -> Option<Glaze> {
    const BAND: i32 = 6;
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return None;
    }
    let average = |pixels: Option<(u32, u32, Vec<u8>)>| -> Option<u32> {
        let rgba = pixels?.2;
        let count = rgba.len() / 4;
        if count == 0 {
            return None;
        }
        let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
        for pixel in rgba.as_chunks::<4>().0 {
            r += u64::from(pixel[0]);
            g += u64::from(pixel[1]);
            b += u64::from(pixel[2]);
        }
        let (r, g, b) = (
            (r / count as u64) as u32,
            (g / count as u64) as u32,
            (b / count as u64) as u32,
        );
        Some((r << 16) | (g << 8) | b)
    };
    let work = super::place::work_area_at(x, y + h);
    let above = (y - BAND >= work.top)
        .then(|| average(capture_rect(x, y - BAND, w as u32, BAND as u32)))
        .flatten();
    let below = (y + h + BAND <= work.bottom)
        .then(|| average(capture_rect(x, y + h, w as u32, BAND as u32)))
        .flatten();
    Some(match (above, below) {
        (Some(top), Some(bottom)) => {
            theme::glaze_from_ambient(theme::srgb(top), theme::srgb(bottom))
        }
        (Some(top), None) => {
            let mut glaze = theme::glaze_from_ambient(theme::srgb(top), theme::srgb(top));
            glaze.edge_bottom = rune::Color::TRANSPARENT;
            glaze
        }
        (None, Some(bottom)) => {
            let mut glaze = theme::glaze_from_ambient(theme::srgb(bottom), theme::srgb(bottom));
            glaze.sheen_top = rune::Color::TRANSPARENT;
            glaze.edge_top = rune::Color::TRANSPARENT;
            glaze
        }
        (None, None) => return None,
    })
}

/// 抓取一块屏幕区域的桌面像素(RGBA,straight alpha)。
#[cfg(windows)]
pub(super) fn capture_rect(x: i32, y: i32, width: u32, height: u32) -> Option<(u32, u32, Vec<u8>)> {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
    };

    if width == 0 || height == 0 {
        return None;
    }
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
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = null_mut();
        let bitmap = match CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0)
        {
            Ok(bitmap) => bitmap,
            Err(_) => {
                let _ = DeleteDC(memory);
                ReleaseDC(None, screen);
                return None;
            }
        };
        let old = SelectObject(memory, bitmap.into());
        let copied = BitBlt(
            memory,
            0,
            0,
            width as i32,
            height as i32,
            Some(screen),
            x,
            y,
            SRCCOPY,
        )
        .is_ok();
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        if copied && !bits.is_null() {
            std::ptr::copy_nonoverlapping(bits.cast::<u8>(), pixels.as_mut_ptr(), pixels.len());
        }
        SelectObject(memory, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        if !copied {
            return None;
        }
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Some((width, height, pixels))
    }
}

#[cfg(not(windows))]
pub(super) fn capture_rect(
    _x: i32,
    _y: i32,
    _width: u32,
    _height: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backdrop_streams_same_size_and_remounts_on_first_or_size_change() {
        let mut slot = None;
        assert!(set_stream(&mut slot, (4, 2), vec![1; 4 * 2 * 4]));
        assert_eq!(slot.as_ref().unwrap().rgba()[0], 1);

        assert!(!set_stream(&mut slot, (4, 2), vec![2; 4 * 2 * 4]));
        assert_eq!(
            slot.as_ref().unwrap().rgba()[0],
            2,
            "same-size set still feeds the mounted pixels"
        );

        assert!(set_stream(&mut slot, (2, 4), vec![3; 2 * 4 * 4]));
        assert_eq!(slot.as_ref().unwrap().size(), (2, 4));
        assert_eq!(slot.as_ref().unwrap().rgba()[0], 3);
    }
}
