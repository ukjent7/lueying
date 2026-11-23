//! 液体玻璃的背板光学:CPU 端对抓回来的桌面像素做位移折射、色散与
//! 提饱和,模糊仍交给 rune 的 `backdrop_blur`(GPU 高斯),亚克力颗粒
//! 由调用方垫在模糊层之上。
//!
//! 位移场按圆角矩形的 SDF 法线 × 圆弧高度剖面采样:边缘带内把采样点
//! 沿外法线向外拉(棱缘外的世界被压进边里),带外中心零位移,棱缘
//! 像素零位移——高度剖面在棱缘附近沿用 sqrt(2t−t²),再乘一条
//! t=0.4→1 的 smoothstep 尾,带内边界处平滑归零。

// 光学的消费方 ui/rune.rs 在 Windows 树里;Linux 上只有测试用得到。
#![cfg_attr(not(windows), allow(dead_code))]

/// 拉距倍率:采样点最远拉出带宽的倍数(光学核心的默认值,调参变体
/// 各自带倍率)。σ4.5-9 的模糊会吃掉一倍带宽的位移,拉距要盖过模糊
/// 才看得见棱缘折弯。无头参数扫描(拉距 1.4/1.8/2.4 × σ7/4 对照图)
/// 定的值:1.8 起可见,2.4 偏闹。产品路径由 `GlassVariant::pull` 传入,
/// 这份常量只给单元测试当默认拉距。
#[cfg(test)]
const PULL: f32 = 2.2;

/// 背板捕获的外扩量(物理像素):盖住最大拉距与色散,棱缘采样才有
/// 原生桌面可采,不落到钳制边上。R 通道比采样点更靠外 1+DISPERSION,
/// 取最远那一路,不是平均拉距。
pub(super) fn capture_pad(band: f32, pull: f32, scale: f32) -> i32 {
    ((band * scale).max(1.0) * pull * (1.0 + DISPERSION)).ceil() as i32 + 2
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

/// 折射高度剖面:带内深度比 `t`(0=棱缘,1=带内边界)换算成拉距倍率。
/// 棱缘附近保留圆弧 sqrt(2t−t²),再乘 t=0.4→1 的 smoothstep 收尾;
/// 两端都归零,带宽边界没有位移跳变。
fn refraction_profile(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let u = ((t - 0.4) / 0.6).clamp(0.0, 1.0);
    let taper = 1.0 - u * u * (3.0 - 2.0 * u);
    (2.0 * t - t * t).sqrt() * taper
}

/// 对外扩 `pad` 物理像素的原图重采样出面板尺寸的处理图。
/// `raw` 是 (w + 2·pad) × (h + 2·pad) 的 RGBA8;`band`、`radius` 都
/// 是物理像素。输出与输入逐位确定:背板静止时结果不变,调用方的
/// 逐帧去重不失效。
/// 测试用的光学入口:按默认拉距 `PULL` 走完整处理链。
#[cfg(test)]
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

/// 光学核心;拉距倍率作参数,产品按配置变体传入,无头调参探针可按它扫描。
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
    // 短面板上原来一个带宽可能越过中心:有效带宽夹到短边一半,折射
    // 只留在各自半边。capture_pad 仍按配置带宽算,保持保守上界。
    let band = band.min(hw.min(hh));
    let mut out = vec![0u8; w as usize * h as usize * 4];
    for py in 0..h as i32 {
        for px in 0..w as i32 {
            let (dist, normal) = edge_field(px as f32 + 0.5, py as f32 + 0.5, hw, hh, radius);
            let depth = -dist;
            let mut rgb = if depth > 0.0 && depth < band {
                // 棱缘 h=0,带内边界 h=0;中段圆弧乘 smoothstep 尾,拉距
                // 峰值不超过一个带宽 × 倍率。
                let t = depth / band;
                let pull = band * pull_scale * refraction_profile(t);
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
                center_pixel(raw, raw_w, raw_h, pad, px, py)
                    .unwrap_or_else(|| tap(raw, raw_w, raw_h, pad, px, py, 0.0, 0.0, 0.0))
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

/// 棱缘环境光的逻辑宽度:从面板外缘向内的窄带。
const RIM_BAND: f32 = 2.0;
/// 棱缘外三档采样的逻辑距离与递减权重(近/中/远)。
const RIM_DISTANCES: [f32; 3] = [2.0, 5.0, 8.0];
const RIM_WEIGHTS: [f32; 3] = [0.5, 0.3, 0.2];
/// 环境棱缘的峰值直通 alpha;真实采样亮度再把它压低。
const RIM_PEAK_ALPHA: f32 = 0.24;
/// 0.25 × 255 向下取整:直通 alpha 的硬上限。
const RIM_MAX_ALPHA: f32 = 63.0;

/// 环境釉缘:复用背板已抓的带 pad 原图,在面板外缘约 2 逻辑像素内
/// 生成一圈直通 alpha 的 RGBA。每个像素从投到棱缘的点沿真实外法线
/// 取近/中/远三档桌面像素(各受 pad 限制,不回采玻璃内),颜色只来自
/// 这些样本;没有有效样本就是全透明,不补白、不镜像对侧。
#[allow(clippy::too_many_arguments)]
pub(super) fn environment_rim(
    raw: &[u8],
    raw_w: u32,
    raw_h: u32,
    pad: i32,
    w: u32,
    h: u32,
    radius: f32,
    scale: f32,
) -> Vec<u8> {
    let mut out = vec![0u8; w as usize * h as usize * 4];
    if w == 0 || h == 0 {
        return out;
    }
    let scale = scale.max(0.01);
    let (hw, hh) = (w as f32 * 0.5, h as f32 * 0.5);
    let radius = radius.min(hw).min(hh);
    let band = (RIM_BAND * scale).min(hw.min(hh));
    for py in 0..h as i32 {
        for px in 0..w as i32 {
            let (dist, normal) = edge_field(px as f32 + 0.5, py as f32 + 0.5, hw, hh, radius);
            let depth = -dist;
            if depth <= 0.0 || depth >= band {
                continue;
            }
            let inner = (depth / band).clamp(0.0, 1.0);
            let fade = {
                let f = 1.0 - inner;
                f * f * (3.0 - 2.0 * f)
            };
            // pad 是面板矩形外的原图外扩预算,从投到棱边的点开始算;
            // 投影占掉的 depth 属于面板内部,不占这份外扩。
            let available = (pad as f32 - 1.0).max(0.0);
            let mut sum = [0.0f32; 3];
            let mut total = 0.0f32;
            for (nominal, weight) in RIM_DISTANCES.into_iter().zip(RIM_WEIGHTS) {
                let reach = nominal * scale;
                if reach <= 0.0 || reach > available {
                    continue;
                }
                let sx = (px + pad) as f32 + normal.0 * (depth + reach);
                let sy = (py + pad) as f32 + normal.1 * (depth + reach);
                if let Some(rgb) = sample_raw(raw, raw_w, raw_h, sx, sy) {
                    for (slot, value) in sum.iter_mut().zip(rgb) {
                        *slot += value * weight;
                    }
                    total += weight;
                }
            }
            if total <= 0.0 {
                continue;
            }
            let rgb = sum.map(|value| value / total);
            // 亮度只定透明度;低亮度软阈值压掉桌面噪点,深色边缘自然熄灭。
            let luminance = (0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]) / 255.0;
            let light = ((luminance - 0.02) / 0.98).clamp(0.0, 1.0);
            let alpha = (RIM_PEAK_ALPHA * fade * light * 255.0)
                .round()
                .clamp(0.0, RIM_MAX_ALPHA) as u8;
            if alpha == 0 {
                continue;
            }
            let at = (py as usize * w as usize + px as usize) * 4;
            out[at] = rgb[0].round().clamp(0.0, 255.0) as u8;
            out[at + 1] = rgb[1].round().clamp(0.0, 255.0) as u8;
            out[at + 2] = rgb[2].round().clamp(0.0, 255.0) as u8;
            out[at + 3] = alpha;
        }
    }
    out
}

/// 可选双线性取样:连续索引落在像素中心坐标系;四邻域任一越界都
/// 视为无样本(调用方明确要求棱缘采样不钳制到边上)。
fn sample_raw(raw: &[u8], raw_w: u32, raw_h: u32, sx: f32, sy: f32) -> Option<[f32; 3]> {
    if sx < 0.0 || sy < 0.0 {
        return None;
    }
    let x0 = sx.floor();
    let y0 = sy.floor();
    let fx = sx - x0;
    let fy = sy - y0;
    let x0 = x0 as i32;
    let y0 = y0 as i32;
    if x0 + 1 >= raw_w as i32 || y0 + 1 >= raw_h as i32 {
        return None;
    }
    let sample = |x: i32, y: i32| -> [f32; 3] {
        let at = ((y as u32 * raw_w + x as u32) * 4) as usize;
        [raw[at] as f32, raw[at + 1] as f32, raw[at + 2] as f32]
    };
    let top = mix(sample(x0, y0), sample(x0 + 1, y0), fx);
    let bottom = mix(sample(x0, y0 + 1), sample(x0 + 1, y0 + 1), fx);
    Some(mix(top, bottom, fy))
}

/// 玻璃背板的高光肩部:在模糊前把过亮的桌面压进连续的可读范围,
/// 暗部与纹理、alpha 原样保留。每个 RGB8 先按标准 sRGB 解码到线性,
/// 取 Rec.709 亮度 L;L ≤ K 不动,否则 L 经软肩到 M:
/// L′ = K + (M−K)·d/(d+M−K), d=L−K。三通道同乘 L′/L 再编码,颜色
/// 比例不变,不做阈值切墨或第二遍桌面抓取。
pub(super) fn prepare_glass_backdrop(rgba: &mut [u8]) {
    const K: f32 = 0.065;
    const M: f32 = 0.12;
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let linear = [
            srgb_decode(pixel[0]),
            srgb_decode(pixel[1]),
            srgb_decode(pixel[2]),
        ];
        let luminance = 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
        if luminance <= K {
            continue;
        }
        let d = luminance - K;
        let shoulder = K + (M - K) * d / (d + M - K);
        let scale = shoulder / luminance;
        for (channel, value) in pixel[..3].iter_mut().zip(linear) {
            *channel = srgb_encode(value * scale);
        }
    }
}

/// 标准 sRGB 传输函数:8-bit 码值到线性光。
fn srgb_decode(value: u8) -> f32 {
    let c = value as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 线性光回到 8-bit sRGB,四舍五入并夹到合法码值。
fn srgb_encode(value: f32) -> u8 {
    let c = value.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
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

/// 未位移像素的直接读取:规范 padded 输入必然命中,不做逐通道
/// 双线性。只有非规范测试输入越界时返回 None,调用方再退回钳制 tap。
fn center_pixel(raw: &[u8], raw_w: u32, raw_h: u32, pad: i32, px: i32, py: i32) -> Option<[u8; 3]> {
    let x = px + pad;
    let y = py + pad;
    if x < 0 || y < 0 || x >= raw_w as i32 || y >= raw_h as i32 {
        return None;
    }
    let at = ((y as u32 * raw_w + x as u32) * 4) as usize;
    let rgb = raw.get(at..at + 3)?;
    Some([rgb[0], rgb[1], rgb[2]])
}

/// 两个 RGB 浮点样本按 `t` 混合。
fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// 原图双线性取样,采样点用像素中心坐标系:连续索引
/// `px + pad + normal·pull` 的整数部分正好落在像素中心,整数取样不
/// 掺入邻居。出界按边缘钳制;物理像素坐标。
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
    let sx = (px + pad) as f32 + nx * pull;
    let sy = (py + pad) as f32 + ny * pull;
    let x0 = sx.floor();
    let y0 = sy.floor();
    let fx = sx - x0;
    let fy = sy - y0;
    let x0 = x0 as i32;
    let y0 = y0 as i32;
    let sample = |x: i32, y: i32| -> [f32; 3] {
        let x = x.clamp(0, raw_w as i32 - 1);
        let y = y.clamp(0, raw_h as i32 - 1);
        let at = ((y as u32 * raw_w + x as u32) * 4) as usize;
        [raw[at] as f32, raw[at + 1] as f32, raw[at + 2] as f32]
    };
    let top = mix(sample(x0, y0), sample(x0 + 1, y0), fx);
    let bottom = mix(sample(x0, y0 + 1), sample(x0 + 1, y0 + 1), fx);
    mix(top, bottom, fy).map(|value| value.round().clamp(0.0, 255.0) as u8)
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
    fn refraction_profile_is_continuous_at_both_band_edges() {
        assert_eq!(refraction_profile(0.0), 0.0);
        assert_eq!(refraction_profile(1.0), 0.0);
        // t=0.4 是 smoothstep 尾的起点:两侧取值连续。
        let at_knee = refraction_profile(0.4);
        let before_knee = refraction_profile(0.4 - 1e-4);
        assert!((at_knee - before_knee).abs() < 1e-3);
        assert!(refraction_profile(1.0 - 1e-3) < 2e-3);
    }

    #[test]
    fn bilinear_tap_is_unbiased_at_pixel_centers_and_blends_fractionally() {
        let raw = [
            10, 20, 30, 255, 100, 110, 120, 255, 200, 210, 220, 255, 250, 250, 250, 255,
        ];
        // 整数索引就是像素中心:pull=0 或整数拉距都不掺邻居。
        assert_eq!(tap(&raw, 4, 1, 0, 1, 0, 0.0, 1.0, 0.0), [100, 110, 120]);
        assert_eq!(tap(&raw, 4, 1, 0, 1, 0, 1.0, 1.0, 0.0), [200, 210, 220]);
        // 半像素位移取两像素中心的等权平均,没有额外半像素偏移。
        assert_eq!(tap(&raw, 4, 1, 0, 1, 0, 0.5, 1.0, 0.0), [150, 160, 170]);
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
    fn band_is_capped_to_short_half_size_across_the_center() {
        // 10×12 面板、带宽 20 远大于短边一半 5:有效带宽夹到 5,
        // 中心像素保持原色,折射不会从两侧跨过中心。
        const RW: u32 = 42;
        const RH: u32 = 44;
        const PAD: i32 = 16;
        let mut raw = vec![0u8; (RW * RH * 4) as usize];
        for (index, pixel) in raw.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let px = (index as u32 % RW) as i32;
            let py = (index as u32 / RW) as i32;
            let inside = px >= PAD && px < RW as i32 - PAD && py >= PAD && py < RH as i32 - PAD;
            pixel[..3].copy_from_slice(if inside { &[64, 64, 64] } else { &[0, 255, 0] });
            pixel[3] = 255;
        }
        let out = process(&raw, RW, RH, PAD, 10, 12, 20.0, 3.0);
        close(at(&out, 10, 5, 6), [64, 64, 64]);
    }

    /// 棱缘测试图:面板外扩 pad 的全黑桌面;`patch` 用原始图像素坐标
    /// 涂一块亮色,其余保持黑色。
    fn rim_raw(
        width: u32,
        height: u32,
        pad: i32,
        patch: Option<(std::ops::Range<i32>, std::ops::Range<i32>)>,
    ) -> (Vec<u8>, u32, u32) {
        let raw_w = width + 2 * pad as u32;
        let raw_h = height + 2 * pad as u32;
        let mut raw = vec![0u8; (raw_w * raw_h * 4) as usize];
        for pixel in raw.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        if let Some((xs, ys)) = patch {
            for y in ys {
                for x in xs.clone() {
                    let at = ((y as u32 * raw_w + x as u32) * 4) as usize;
                    raw[at..at + 4].copy_from_slice(&[220, 60, 40, 255]);
                }
            }
        }
        (raw, raw_w, raw_h)
    }

    fn rim_alpha(out: &[u8], w: u32, x: u32, y: u32) -> u8 {
        out[((y * w + x) * 4 + 3) as usize]
    }

    #[test]
    fn environment_rim_follows_local_patch_and_clears_when_gone() {
        const W: u32 = 48;
        const H: u32 = 30;
        const PAD: i32 = 18;
        let (raw, raw_w, raw_h) = rim_raw(W, H, PAD, Some((32..52, 6..18)));
        let out = environment_rim(&raw, raw_w, raw_h, PAD, W, H, 8.0, 1.0);
        assert!(
            rim_alpha(&out, W, 24, 0) > 4,
            "top rim should pick up the nearby patch"
        );
        assert!(
            out.as_chunks::<4>().0.iter().all(|pixel| pixel[3] <= 63),
            "rim alpha must stay at or below 0.25"
        );
        let at = (24 * 4) as usize;
        assert!(
            out[at] > 100 && out[at + 2] < 100,
            "color must come from the patch"
        );
        assert_eq!(
            rim_alpha(&out, W, 24, H - 1),
            0,
            "opposite rim has no source and must stay dark"
        );
        let (dark, dark_w, dark_h) = rim_raw(W, H, PAD, None);
        let empty = environment_rim(&dark, dark_w, dark_h, PAD, W, H, 8.0, 1.0);
        assert!(
            empty.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0),
            "removing the patch must leave the whole rim transparent"
        );
    }

    #[test]
    fn environment_rim_lights_the_nearest_side_only() {
        const W: u32 = 48;
        const H: u32 = 30;
        const PAD: i32 = 18;
        let (raw, raw_w, raw_h) = rim_raw(W, H, PAD, Some((2..14, 28..42)));
        let out = environment_rim(&raw, raw_w, raw_h, PAD, W, H, 8.0, 1.0);
        assert!(
            rim_alpha(&out, W, 0, 15) > 4,
            "left rim should pick up the left-side patch"
        );
        assert_eq!(
            rim_alpha(&out, W, W - 1, 15),
            0,
            "right rim must not fabricate the opposite side"
        );
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

    fn luminance(rgb: [u8; 3]) -> f32 {
        0.2126 * srgb_decode(rgb[0]) + 0.7152 * srgb_decode(rgb[1]) + 0.0722 * srgb_decode(rgb[2])
    }

    #[test]
    fn highlight_shoulder_caps_white_and_bright_colors_near_linear_012() {
        let cases = [[255, 255, 255], [255, 64, 0], [0, 255, 0], [255, 0, 255]];
        for rgb in cases {
            let mut rgba = [rgb[0], rgb[1], rgb[2], 173];
            prepare_glass_backdrop(&mut rgba);
            assert_eq!(rgba[3], 173, "alpha 必须原样保留");
            let lit = luminance([rgba[0], rgba[1], rgba[2]]);
            assert!(lit <= 0.12 + 0.002, "{rgb:?} 压肩后亮度 {lit} 仍高于目标");
        }
        let mut white = [255, 255, 255, 255];
        prepare_glass_backdrop(&mut white);
        assert!(
            luminance([white[0], white[1], white[2]]) > 0.11,
            "白色应保留在肩部附近,不能整块压黑"
        );
    }

    #[test]
    fn shadow_colors_are_untouched() {
        let mut rgba = [10, 12, 14, 37];
        prepare_glass_backdrop(&mut rgba);
        assert_eq!(rgba, [10, 12, 14, 37]);
        let mut mid = [48, 50, 52, 200];
        let original = mid;
        prepare_glass_backdrop(&mut mid);
        assert_eq!(mid, original);
    }

    #[test]
    fn capture_pad_covers_the_farthest_dispersed_tap() {
        for (band, pull, scale) in [
            (8.0_f32, 2.2_f32, 1.0_f32),
            (12.0, 3.0, 1.25),
            (1.0, 2.2, 2.0),
        ] {
            let farthest = (band * scale).max(1.0) * pull * (1.0 + DISPERSION);
            let pad = capture_pad(band, pull, scale) as f32;
            assert!(
                pad >= farthest,
                "pad {pad} 没盖住最远位移 {farthest} (band {band}, pull {pull}, scale {scale})"
            );
        }
    }
}
