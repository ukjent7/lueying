//! 液体玻璃的背板光学:CPU 端对抓回来的桌面像素做位移折射、色散与
//! 提饱和,模糊仍交给 rune 的 `backdrop_blur`(GPU 高斯),亚克力颗粒
//! 由调用方垫在模糊层之上。
//!
//! 位移场按圆角矩形的 SDF 法线 × 圆弧高度剖面采样:边缘带内把采样点
//! 沿外法线向外拉(棱缘外的世界被压进边里),带外中心零位移,棱缘
//! 像素零位移——与 Web 参考配方一致(高度剖面 sqrt(2t−t²) 保证边缘
//! h=0,带宽 ≤ 0.35 × 短边一半)。

// 光学的消费方 ui/rune.rs 在 Windows 树里;Linux 上只有测试用得到。
#![cfg_attr(not(windows), allow(dead_code))]

/// 拉距倍率:采样点最远拉出带宽的倍数(光学核心的默认值,调参变体
/// 各自带倍率)。σ4.5-9 的模糊会吃掉一倍带宽的位移,拉距要盖过模糊
/// 才看得见棱缘折弯。无头参数扫描(拉距 1.4/1.8/2.4 × σ7/4 对照图)
/// 定的值:1.8 起可见,2.4 偏闹。
const PULL: f32 = 2.2;

/// 背板捕获的外扩量(物理像素):盖住最大拉距,棱缘采样才有原生
/// 桌面可采,不落到钳制边上。
pub(super) fn capture_pad(band: f32, pull: f32, scale: f32) -> i32 {
    (band * pull * scale).ceil() as i32 + 2
}

/// 临时调参变体(用户在 config.toml 的 `ui_glass_variant` 里选
/// A/B/C/D);选定后落常数并移除。
#[derive(Clone, Copy)]
pub(super) struct GlassVariant {
    /// 折射带宽(逻辑像素):参考配方上限是 0.35 × 短边一半(条上
    /// 7px),但 σ4.5+ 的模糊会把细环整个抹平(用户实测确认),放宽
    /// 到 10-16 才立得住。
    pub(super) band: f32,
    /// 拉距倍率。
    pub(super) pull: f32,
    /// 高透端的高斯模糊 σ;磨砂端恒为 FROST_BLUR。
    pub(super) base_blur: f32,
    /// 高透端的面层 tint alpha;磨砂端恒为 0.62。
    pub(super) base_tint: f32,
    /// 描边 alpha。
    pub(super) border: f32,
    /// 是否画加深的底部内阴影(meniscus)。
    pub(super) meniscus: bool,
}

pub(super) fn variant(name: &str) -> GlassVariant {
    match name {
        "B" => GlassVariant {
            band: 8.0,
            pull: 2.2,
            base_blur: 3.5,
            base_tint: 0.28,
            border: 0.14,
            meniscus: false,
        },
        "C" => GlassVariant {
            band: 12.0,
            pull: 3.0,
            base_blur: 4.5,
            base_tint: 0.40,
            border: 0.24,
            meniscus: true,
        },
        "D" => GlassVariant {
            band: 12.0,
            pull: 3.0,
            base_blur: 3.5,
            base_tint: 0.28,
            border: 0.24,
            meniscus: true,
        },
        _ => GlassVariant {
            band: 8.0,
            pull: 2.2,
            base_blur: 4.5,
            base_tint: 0.40,
            border: 0.14,
            meniscus: false,
        },
    }
}
/// 色散:R 比采样点更靠外、B 更靠内,偏移占折射向量的比例。
const DISPERSION: f32 = 0.35;
/// 提饱和(vibrancy):Rec.709 亮度向原色插值的倍率。
const VIBRANCY: f32 = 1.28;
/// 亚克力颗粒幅度(0..=255 的明暗抖动半幅)。
pub(super) const GRAIN: u8 = 2;

/// 对外扩 `pad` 物理像素的原图重采样出面板尺寸的处理图。
/// `raw` 是 (w + 2·pad) × (h + 2·pad) 的 RGBA8;`band`、`radius` 都
/// 是物理像素。输出与输入逐位确定:背板静止时结果不变,调用方的
/// 逐帧去重不失效。
#[allow(clippy::too_many_arguments)]
pub(super) fn process(
    raw: &[u8],
    raw_w: u32,
    raw_h: u32,
    pad: i32,
    w: u32,
    h: u32,
    band: f32,
    radius: f32,
) -> Vec<u8> {
    process_with_pull(raw, raw_w, raw_h, pad, w, h, band, radius, PULL)
}

/// 光学核心;拉距倍率作参数,无头调参探针按它扫描,产品走 [`PULL`]。
#[allow(clippy::too_many_arguments)]
pub(super) fn process_with_pull(
    raw: &[u8],
    raw_w: u32,
    raw_h: u32,
    pad: i32,
    w: u32,
    h: u32,
    band: f32,
    radius: f32,
    pull_scale: f32,
) -> Vec<u8> {
    let band = band.max(1.0);
    let radius = radius.min(w as f32 * 0.5).min(h as f32 * 0.5);
    let (hw, hh) = (w as f32 * 0.5, h as f32 * 0.5);
    let mut out = vec![0u8; w as usize * h as usize * 4];
    for py in 0..h as i32 {
        for px in 0..w as i32 {
            let (dist, normal) = edge_field(px as f32 + 0.5, py as f32 + 0.5, hw, hh, radius);
            let depth = -dist;
            let mut rgb = if depth > 0.0 && depth < band {
                // 棱缘 h=0,带内边界 h=1;拉距最深一个带宽 × 倍率。
                let t = depth / band;
                let pull = band * pull_scale * (2.0 * t - t * t).sqrt();
                let (nx, ny) = normal;
                // 色散:R 更靠外、B 更靠内,各自取自己通道的采样。
                let red = tap(
                    raw,
                    raw_w,
                    raw_h,
                    pad,
                    px,
                    py,
                    pull * (1.0 + DISPERSION),
                    nx,
                    ny,
                );
                let green = tap(raw, raw_w, raw_h, pad, px, py, pull, nx, ny);
                let blue = tap(
                    raw,
                    raw_w,
                    raw_h,
                    pad,
                    px,
                    py,
                    pull * (1.0 - DISPERSION),
                    nx,
                    ny,
                );
                [red[0], green[1], blue[2]]
            } else {
                tap(raw, raw_w, raw_h, pad, px, py, 0.0, 0.0, 0.0)
            };
            let lum = 0.2126 * rgb[0] as f32 + 0.7152 * rgb[1] as f32 + 0.0722 * rgb[2] as f32;
            let grain = grain(px, py);
            for channel in &mut rgb {
                let vivid = lum + (*channel as f32 - lum) * VIBRANCY + grain;
                *channel = vivid.round().clamp(0.0, 255.0) as u8;
            }
            let at = (py as usize * w as usize + px as usize) * 4;
            out[at..at + 3].copy_from_slice(&rgb);
            out[at + 3] = 255;
        }
    }
    out
}

/// 面板尺寸的固定颗粒图:白与黑各半的低 alpha 抖动,垫在模糊层上
/// 才不会被 backdrop_blur 抹掉。同一尺寸只生成一次。
pub(super) fn grain_rgba(w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; w as usize * h as usize * 4];
    for (index, pixel) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let px = (index as u32 % w) as i32;
        let py = (index as u32 / w) as i32;
        let n = (px.wrapping_mul(73856093) ^ py.wrapping_mul(19349663)).wrapping_mul(83492791);
        // 白与黑各半:亮度抖动 ±GRAIN,色相不动。
        let bits = n as u32;
        let value = u8::from(bits >> 16 & 1 == 1) * 255;
        let alpha = ((bits >> 8) & 0xff) % u32::from(GRAIN) + 1;
        *pixel = [value, value, value, alpha as u8];
    }
    out
}

/// 圆角矩形 SDF(负值在内部)与单位外法线;物理像素,盒心在原点。
fn edge_field(px: f32, py: f32, hw: f32, hh: f32, radius: f32) -> (f32, (f32, f32)) {
    let r = radius.min(hw).min(hh);
    let dx = (px - hw).abs() - (hw - r);
    let dy = (py - hh).abs() - (hh - r);
    let dist = (dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0)) - r;
    let sx = (px - hw).signum();
    let sy = (py - hh).signum();
    let normal = if dx > 0.0 && dy > 0.0 {
        let (mx, my) = (sx * dx, sy * dy);
        let len = (mx * mx + my * my).sqrt();
        (mx / len, my / len)
    } else if dx >= dy {
        (sx, 0.0)
    } else {
        (0.0, sy)
    };
    (dist, normal)
}

/// 原图最近邻取样,采样点出界按边缘钳制;物理像素坐标。
#[allow(clippy::too_many_arguments)]
fn tap(
    raw: &[u8],
    raw_w: u32,
    raw_h: u32,
    pad: i32,
    px: i32,
    py: i32,
    pull: f32,
    nx: f32,
    ny: f32,
) -> [u8; 3] {
    let sx = (px + pad) as f32 + 0.5 + nx * pull;
    let sy = (py + pad) as f32 + 0.5 + ny * pull;
    let ix = (sx.floor() as i32).clamp(0, raw_w as i32 - 1);
    let iy = (sy.floor() as i32).clamp(0, raw_h as i32 - 1);
    let at = ((iy as u32 * raw_w + ix as u32) * 4) as usize;
    [raw[at], raw[at + 1], raw[at + 2]]
}

/// 固定图案噪点:同一坐标永远同一值,幅度 ±GRAIN。
fn grain(px: i32, py: i32) -> f32 {
    let n = (px
        .wrapping_mul(73856093)
        .wrapping_add(py.wrapping_mul(19349663)))
    .wrapping_mul(83492791);
    let spread = ((n >> 16) & 0xff) as f32 / 255.0;
    spread * 2.0 * f32::from(GRAIN) - f32::from(GRAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试夹具:10×10 面板,pad=13;面板区填 `inner`,外圈按离面板
    /// 的距离分层——5px 内 `near`(与面板同色),更远 `far`。拉距 2.2
    /// 后 R/B 采样点会伸进外圈,色散断言靠远/近两层的通道差。
    fn padded(inner: [u8; 3], near: [u8; 3], far: [u8; 3]) -> Vec<u8> {
        const RW: u32 = 36;
        const RH: u32 = 36;
        const PAD: i32 = 13;
        let mut raw = vec![0u8; (RW * RH * 4) as usize];
        for (index, pixel) in raw.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let px = (index as u32 % RW) as i32;
            let py = (index as u32 / RW) as i32;
            let inside = px >= PAD && px < RW as i32 - PAD && py >= PAD && py < RH as i32 - PAD;
            let color = if inside {
                inner
            } else {
                let dx = (PAD - px).max(px - (RW as i32 - 1 - PAD)).max(0);
                let dy = (PAD - py).max(py - (RH as i32 - 1 - PAD)).max(0);
                if dx.max(dy) >= 5 { far } else { near }
            };
            pixel[..3].copy_from_slice(&color);
            pixel[3] = 255;
        }
        raw
    }

    fn at(image: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * w + x) * 4) as usize;
        [image[at], image[at + 1], image[at + 2]]
    }

    /// 颗粒有 ±GRAIN 的扰动,断言按容差比。
    fn close(actual: [u8; 3], expected: [u8; 3]) {
        for (a, e) in actual.iter().zip(expected) {
            let diff = (i16::from(*a) - i16::from(e)).abs();
            assert!(diff <= i16::from(GRAIN), "{actual:?} vs {expected:?}");
        }
    }

    #[test]
    fn center_is_untouched_and_saturated() {
        // 中性灰不参与提饱和:中心像素应等于输入(±颗粒)。
        let raw = padded([128, 128, 128], [0, 0, 255], [255, 0, 0]);
        let out = process(&raw, 36, 36, 13, 10, 10, 4.0, 3.0);
        close(at(&out, 10, 5, 5), [128, 128, 128]);
    }

    #[test]
    fn edge_pulls_from_outside() {
        // 棱缘采样点被拉出面板:远环纯红进到边里,中心保持灰。
        let raw = padded([128, 128, 128], [0, 0, 255], [255, 0, 0]);
        let out = process(&raw, 36, 36, 13, 10, 10, 4.0, 3.0);
        let edge = at(&out, 10, 5, 1);
        assert!(
            edge[0] > 200,
            "top rim should pull red from outside: {edge:?}"
        );
        close(at(&out, 10, 5, 5), [128, 128, 128]);
    }

    #[test]
    fn dispersion_splits_channels() {
        // R 比采样点更靠外:远环红、中环蓝,棱缘红蓝混成品红。
        let raw = padded([0, 0, 255], [0, 0, 255], [255, 0, 0]);
        let out = process(&raw, 36, 36, 13, 10, 10, 4.0, 3.0);
        let edge = at(&out, 10, 5, 1);
        assert!(
            edge[0] > 100 && edge[2] > 100,
            "rim should mix red (far) and blue (near): {edge:?}"
        );
    }

    #[test]
    fn same_input_same_output() {
        let raw = padded([128, 128, 128], [0, 0, 255], [255, 0, 0]);
        let a = process(&raw, 36, 36, 13, 10, 10, 4.0, 3.0);
        let b = process(&raw, 36, 36, 13, 10, 10, 4.0, 3.0);
        assert_eq!(a, b);
    }

    #[test]
    fn radius_clamped_to_half_size() {
        // 半径大于短边一半时不越界:细长面板有带外中心,照常出图。
        const RW: u32 = 32;
        const RH: u32 = 34;
        const PAD: i32 = 11;
        let mut raw = vec![0u8; (RW * RH * 4) as usize];
        for (index, pixel) in raw.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let px = (index as u32 % RW) as i32;
            let py = (index as u32 / RW) as i32;
            let inside = px >= PAD && px < RW as i32 - PAD && py >= PAD && py < RH as i32 - PAD;
            pixel[..3].copy_from_slice(if inside { &[64, 64, 64] } else { &[0, 255, 0] });
            pixel[3] = 255;
        }
        let out = process(&raw, RW, RH, PAD, 10, 12, 3.0, 8.0);
        assert_eq!(out.len(), 10 * 12 * 4);
        close(at(&out, 10, 5, 6), [64, 64, 64]);
    }

    #[test]
    fn grain_map_is_deterministic_and_subtle() {
        let a = grain_rgba(7, 5);
        let b = grain_rgba(7, 5);
        assert_eq!(a, b);
        assert_eq!(a.len(), 7 * 5 * 4);
        for pixel in a.as_chunks::<4>().0 {
            assert!(pixel[3] >= 1 && pixel[3] <= GRAIN);
            assert!(pixel[0] == pixel[1] && pixel[1] == pixel[2]);
        }
    }
}
