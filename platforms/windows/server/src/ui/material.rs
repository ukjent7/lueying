//! 候选窗与工具栏共用的材质:屏幕采样、玻璃面配方与真实环境釉光。
//!
//! 玻璃不是一层透明色:背后是 BitBlt 抓到的桌面像素,过 `liquid`
//! 的折射/色散/提饱和后铺在窗口裁切层下。这里只放纯采样与共用
//! 渲染构件,窗口摆位、节流与生命周期由各自 overlay 管理。

use lueying_render::panel::Palette;
use lueying_server::theme::{self, Glaze};
use rune::{Align, El, OverlayHandle, OverlaySupport, Pixels, SurfaceFit, el};

use crate::liquid;

/// 磨砂玻璃在雾化端的模糊强度;高透端的 σ 由变体给出。
pub(super) const FROST_BLUR: f32 = 9.0;

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

/// 0xRRGGBB + 线性透明度。
fn rgba(v: u32, a: f32) -> rune::Color {
    theme::srgb_alpha(v, a)
}

/// 玻璃面:模糊、面层 tint、上下压光与边框。候选窗与工具栏共用同一份
/// 像素配方;meniscus、颗粒与釉缘是各自按变体追加的层。
pub(super) fn glass_face(face: El, variant: liquid::GlassVariant, frost: f32, stroke: f32) -> El {
    let frost = frost.clamp(0.0, 1.0);
    let frost_blur = variant.base_blur + (FROST_BLUR - variant.base_blur) * frost;
    let frost_fill = rgba(
        0x262624,
        variant.base_tint + (0.62 - variant.base_tint) * frost,
    );
    let frost_top = rgba(0x000000, 0.06 + 0.04 * frost);
    let frost_bottom = rgba(0x000000, 0.22 + 0.08 * frost);
    face.backdrop_blur(frost_blur)
        .fill(frost_fill)
        .gradient(frost_top, frost_bottom)
        .border(1.0 * stroke, rgba(0x18191b, variant.border))
}

/// 变体 C/D 的底部 meniscus 内阴影:贴底边,形变时跟着底缘走。
pub(super) fn glass_meniscus(face: El, variant: liquid::GlassVariant, stroke: f32) -> El {
    if variant.meniscus {
        face.child(
            el().pin(Some(0.0), None, Some(0.0), Some(0.0))
                .h(2.0 * stroke)
                .fill(rgba(0x000000, 0.28)),
        )
    } else {
        face
    }
}

/// 亚克力颗粒:固定噪点图垫在模糊之上,1:1 不随面板形变拉伸。
pub(super) fn glass_grain(face: El, grain: &Pixels, above: bool) -> El {
    face.child(
        el().absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .pixels_fit(grain, SurfaceFit::Fill)
            .surface_unscaled(true)
            .surface_anchor(Align::Start, if above { Align::End } else { Align::Start }),
    )
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
