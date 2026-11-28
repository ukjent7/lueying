//! 候选窗与工具栏共用的材质:屏幕采样、连续圆角裁切与真实环境釉光。
//!
//! 玻璃的桌面像素经 BitBlt 原样抓进窗口(抓屏薄层),折射/色散/
//! frost 全部在 rune-fx lens 的 GPU 着色器里。这里只放纯采样与
//! 共用渲染构件,窗口摆位、节流与生命周期由各自 overlay 管理。

use lueying_render::panel::Palette;
use lueying_server::theme::{self};
use rune::{El, OverlayHandle, OverlaySupport, Pixels};

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
/// `Pixels::replace` 原地换像素,不需要重跑 view,故返回 false;首次
/// 挂载或尺寸变化仍要重建,返回 true。返回的旧 buffer 留给下一次抓屏
/// 复用,避免逐帧分配。
pub(super) fn set_stream(
    slot: &mut Option<Pixels>,
    size: (u32, u32),
    rgba: Vec<u8>,
) -> (bool, Vec<u8>) {
    let mounted = slot.as_ref().is_some_and(|pixels| pixels.size() == size);
    let old = slot
        .get_or_insert_with(Pixels::new)
        .replace(size.0, size.1, rgba);
    (!mounted, old)
}

/// 面板外壳与玻璃背板共用同一个连续圆角与裁切。
pub(super) fn rounded_surface(el: El, radius: f32) -> El {
    el.radius(radius).continuous_corners().clip_rounded(radius)
}

/// 抓屏外扩环的环境亮度(sRGB 0..1):pad 带就是面板外的真实桌面
/// 像素,四条边隔点采样求 luma 均值,量化到 0.05 挡逐帧微抖。
/// BitBlt 读屏幕外的区域为黑,条体贴屏幕边缘时会略低估。
pub(super) fn ring_luma(raw: &[u8], w: u32, h: u32, pad: i32) -> f32 {
    let (w, h) = (w as usize, h as usize);
    let pad = (pad.max(0) as usize).min(w / 2).min(h / 2);
    if pad == 0 || w == 0 {
        return 0.5;
    }
    let mut sum = 0.0f32;
    let mut n = 0u32;
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            if y < pad || y >= h - pad || x < pad || x >= w - pad {
                let i = (y * w + x) * 4;
                sum += 0.2126 * raw[i] as f32
                    + 0.7152 * raw[i + 1] as f32
                    + 0.0722 * raw[i + 2] as f32;
                n += 1;
            }
        }
    }
    if n == 0 {
        return 0.5;
    }
    let luma = sum / (n as f32 * 255.0);
    (luma * 20.0).round() / 20.0
}

/// 黑瓷面:不透的共享石墨胎体配结构描边;釉光只来自真实环境采样,
/// 采不到保持哑光。`height` 是面板逻辑高度,决定顶部光泽的渐隐范围。
pub(super) fn ceramic_face(face: El, palette: &Palette, stroke: f32) -> El {
    face.fill(theme::srgb(palette.bg))
        .border(1.0 * stroke, theme::srgb(palette.border))
}

/// 抓取一块屏幕区域的桌面像素(RGBA,straight alpha)。
#[cfg(windows)]
pub(super) fn capture_rect(
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    mut reuse: Vec<u8>,
) -> Option<(u32, u32, Vec<u8>)> {
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
        let len = (width * height * 4) as usize;
        reuse.resize(len, 0);
        if copied && !bits.is_null() {
            std::ptr::copy_nonoverlapping(bits.cast::<u8>(), reuse.as_mut_ptr(), len);
        }
        SelectObject(memory, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        if !copied {
            return None;
        }
        for pixel in reuse.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
            pixel[3] = 255;
        }
        Some((width, height, reuse))
    }
}

#[cfg(not(windows))]
pub(super) fn capture_rect(
    _x: i32,
    _y: i32,
    _width: u32,
    _height: u32,
    _reuse: Vec<u8>,
) -> Option<(u32, u32, Vec<u8>)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backdrop_streams_same_size_and_remounts_on_first_or_size_change() {
        let mut slot = None;
        let (mounted, old) = set_stream(&mut slot, (4, 2), vec![1; 4 * 2 * 4]);
        assert!(mounted);
        assert!(old.is_empty());
        assert_eq!(slot.as_ref().unwrap().rgba()[0], 1);

        let (mounted, old) = set_stream(&mut slot, (4, 2), vec![2; 4 * 2 * 4]);
        assert!(!mounted);
        assert_eq!(old.len(), 4 * 2 * 4, "previous frame comes back for reuse");
        assert_eq!(
            slot.as_ref().unwrap().rgba()[0],
            2,
            "same-size replace still feeds the mounted pixels"
        );

        let (mounted, _) = set_stream(&mut slot, (2, 4), vec![3; 2 * 4 * 4]);
        assert!(mounted);
        assert_eq!(slot.as_ref().unwrap().size(), (2, 4));
        assert_eq!(slot.as_ref().unwrap().rgba()[0], 3);
    }

    #[test]
    fn ring_luma_reads_only_the_pad_band() {
        // 8x8 图、pad 2:外环全白、面板区全黑,均值应为 ~1。
        let (w, h, pad) = (8u32, 8u32, 2i32);
        let mut raw = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                if y < 2 || y >= 6 || x < 2 || x >= 6 {
                    let i = (y * w as usize + x) * 4;
                    raw[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        let luma = ring_luma(&raw, w, h, pad);
        assert!(luma > 0.9, "outer ring is white: {luma}");
        // 反相:外环黑、面板白。
        for px in raw.chunks_exact_mut(4) {
            px[0] = 255 - px[0];
            px[1] = 255 - px[1];
            px[2] = 255 - px[2];
        }
        let luma = ring_luma(&raw, w, h, pad);
        assert!(luma < 0.1, "outer ring is black: {luma}");
        // 零 pad 退化到中灰兜底。
        assert_eq!(ring_luma(&raw, w, h, 0), 0.5);
    }
}
