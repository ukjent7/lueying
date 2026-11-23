//! 设置窗黑瓷外壳的影棚灯棱线。
//!
//! 一盏固定的影棚灯斜照窗口周长的倒角,只在外壳四边留下很窄的中性灰
//! 反光。所有边共用同一份 `AreaLight`、表面几何与距离衰减公式;不读
//! 屏幕、不跟时间、不接候选窗或工具栏。渲染用四条一次性算好的小 RGBA
//! 光带,挂在外壳控件之下。

/// 三维点/向量(归一化窗口坐标)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn length_squared(self) -> f32 {
        self.dot(self)
    }

    fn normalized(self) -> Self {
        let length = self.length_squared().sqrt().max(1e-6);
        Self::new(self.x / length, self.y / length, self.z / length)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

/// 固定影棚灯(区域光):归一化窗口坐标 x/y ∈ [-0.5, 0.5](y 向下),
/// z 朝向观察者;`power` 是中性标量强度。有限面积用横向三个偏移的
/// 加权采样近似,权重 1/4、1/2、1/4。
#[derive(Clone, Copy, Debug)]
pub(crate) struct AreaLight {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub power: f32,
    pub half_width: f32,
}

/// 设置窗外壳的固定影棚灯:左上方、屏幕前;强度中性,不随主题/日期变化。
pub(crate) const AREA_LIGHT: AreaLight = AreaLight {
    x: -0.24,
    y: -0.22,
    z: 0.35,
    power: 1.0,
    half_width: 0.16,
};

/// 窗口周长的一条接收边。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Top,
    Right,
    Bottom,
    Left,
}

impl Edge {
    pub(crate) const ALL: [Edge; 4] = [Edge::Top, Edge::Right, Edge::Bottom, Edge::Left];

    /// 外法线在窗口平面内的方向(y 向下)。
    fn outward(self) -> Vec3 {
        match self {
            Edge::Top => Vec3::new(0.0, -1.0, 0.0),
            Edge::Right => Vec3::new(1.0, 0.0, 0.0),
            Edge::Bottom => Vec3::new(0.0, 1.0, 0.0),
            Edge::Left => Vec3::new(-1.0, 0.0, 0.0),
        }
    }
}

/// 倒角在归一化坐标里的进深;只影响法线取样位置,不代表实际像素厚度。
const BEVEL_DEPTH: f32 = 0.012;

/// 周长采样点:u 沿边 0..1;v 从外缘向内 0..1。
fn surface(edge: Edge, u: f32, v: f32) -> Vec3 {
    let u = u.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0) * BEVEL_DEPTH;
    match edge {
        Edge::Top => Vec3::new(-0.5 + u, -0.5 + v, 0.0),
        Edge::Right => Vec3::new(0.5 - v, -0.5 + u, 0.0),
        Edge::Bottom => Vec3::new(-0.5 + u, 0.5 - v, 0.0),
        Edge::Left => Vec3::new(-0.5 + v, -0.5 + u, 0.0),
    }
}

/// 倒角表面法线:外缘更朝外,向内逐渐转向观察者;两段都归一到单位长。
fn surface_normal(edge: Edge, v: f32) -> Vec3 {
    let v = v.clamp(0.0, 1.0);
    let outward = edge.outward();
    let side = 1.0 - 0.45 * v;
    Vec3::new(outward.x * side, outward.y * side, 0.55 + 0.45 * v).normalized()
}

/// 平滑阶跃,用于倒角两端的软收尾。
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 横截面上的软 taper:外缘与内缘都平滑到 0,中间最亮。
fn bevel_taper(v: f32) -> f32 {
    smoothstep(0.0, 0.3, v) * (1.0 - smoothstep(0.7, 1.0, v))
}

/// 单点反光增益:`power · max(dot(n,l),0) / (1 + 4·d²)`,再乘倒角
/// 软 taper;有限面积对灯具横向三个位置加权平均。所有边共用这一份。
pub(crate) fn light_gain(light: AreaLight, edge: Edge, u: f32, v: f32) -> f32 {
    let point = surface(edge, u, v);
    let normal = surface_normal(edge, v);
    let mut sum = 0.0;
    for (offset, weight) in [(-1.0, 0.25), (0.0, 0.5), (1.0, 0.25)] {
        let lamp = Vec3::new(light.x + offset * light.half_width, light.y, light.z);
        let to_lamp = lamp - point;
        let distance_squared = to_lamp.length_squared();
        let distance = distance_squared.sqrt().max(1e-6);
        let dot = normal.dot(to_lamp) / distance;
        if dot > 0.0 {
            sum += weight * dot / (1.0 + 4.0 * distance_squared);
        }
    }
    (light.power.max(0.0) * sum * bevel_taper(v)).max(0.0)
}

/// 光带纹理的纵向/横截面采样数;小图一次上传,绘制时拉满边长。
pub(crate) const LENGTH_SAMPLES: u32 = 96;
pub(crate) const CROSS_SAMPLES: u32 = 8;
/// 光带在窗口上的逻辑厚度。
pub(crate) const STRIP_THICKNESS: f32 = 2.0;
/// 中性灰的受光色;透明度才由光强决定。
const RIM_RGB: [u8; 3] = [188, 188, 188];
/// 受限的峰值 alpha:即使正对光源也只留一层很薄的反光。
const PEAK_ALPHA: f32 = 0.18;

/// 生成一条边的直通 alpha RGBA 光带。顶/底边返回 (长, 厚),
/// 左/右边返回 (厚, 长);像素颜色中性灰,alpha 完全由光照公式算。
pub(crate) fn strip_rgba(
    light: AreaLight,
    edge: Edge,
    length: u32,
    cross: u32,
) -> (u32, u32, Vec<u8>) {
    let horizontal = matches!(edge, Edge::Top | Edge::Bottom);
    let (w, h) = if horizontal {
        (length, cross)
    } else {
        (cross, length)
    };
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (u, v) = match edge {
                Edge::Top => ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32),
                Edge::Bottom => (
                    (x as f32 + 0.5) / w as f32,
                    (h as f32 - 0.5 - y as f32) / h as f32,
                ),
                Edge::Left => ((y as f32 + 0.5) / h as f32, (x as f32 + 0.5) / w as f32),
                Edge::Right => (
                    (y as f32 + 0.5) / h as f32,
                    (w as f32 - 0.5 - x as f32) / w as f32,
                ),
            };
            let gain = light_gain(light, edge, u, v);
            // 无硬平台:零增益严格为零,强增益有界,近处仍比远处亮。
            let alpha = PEAK_ALPHA * gain / (gain + 0.25);
            let byte = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
            let at = ((y * w + x) * 4) as usize;
            if byte > 0 {
                out[at..at + 3].copy_from_slice(&RIM_RGB);
                out[at + 3] = byte;
            }
        }
    }
    (w, h, out)
}

/// 设置窗外壳的影棚灯棱线层:四条 2 逻辑像素的光带,垫在页头/正文/页脚
/// 之下;高对比度由调用方整层省略。纹理用 `OnceLock` 只算一次。
#[cfg(windows)]
pub(crate) fn layer() -> rune::El {
    use rune::el;

    struct Strips {
        top: rune::Pixels,
        right: rune::Pixels,
        bottom: rune::Pixels,
        left: rune::Pixels,
    }

    fn strips() -> &'static Strips {
        use std::sync::OnceLock;

        static STRIPS: OnceLock<Strips> = OnceLock::new();
        STRIPS.get_or_init(|| {
            let [top, right, bottom, left] = Edge::ALL.map(|edge| {
                let (w, h, rgba) = strip_rgba(AREA_LIGHT, edge, LENGTH_SAMPLES, CROSS_SAMPLES);
                let pixels = rune::Pixels::new();
                pixels.set(w, h, rgba);
                pixels
            });
            Strips {
                top,
                right,
                bottom,
                left,
            }
        })
    }

    let strips = strips();
    let t = STRIP_THICKNESS;
    el().key("ceramic-rim")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .a11y_hidden()
        .child(
            el().key("ceramic-rim-top")
                .pin(Some(0.0), Some(0.0), Some(0.0), None)
                .w_full()
                .h(t)
                .pixels(&strips.top)
                .a11y_hidden(),
        )
        .child(
            el().key("ceramic-rim-bottom")
                .pin(Some(0.0), None, Some(0.0), Some(0.0))
                .w_full()
                .h(t)
                .pixels(&strips.bottom)
                .a11y_hidden(),
        )
        .child(
            el().key("ceramic-rim-left")
                .pin(Some(0.0), Some(0.0), None, Some(0.0))
                .w(t)
                .h_full()
                .pixels(&strips.left)
                .a11y_hidden(),
        )
        .child(
            el().key("ceramic-rim-right")
                .pin(None, Some(0.0), Some(0.0), Some(0.0))
                .w(t)
                .h_full()
                .pixels(&strips.right)
                .a11y_hidden(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_lamp_horizontally_shifts_reflection_to_that_side() {
        let lamp_right = AreaLight {
            x: 0.24,
            ..AREA_LIGHT
        };
        let lamp_left = AreaLight {
            x: -0.24,
            ..AREA_LIGHT
        };
        let right_lit_by_right = light_gain(lamp_right, Edge::Right, 0.5, 0.65);
        let right_lit_by_left = light_gain(lamp_left, Edge::Right, 0.5, 0.65);
        assert!(
            right_lit_by_right > right_lit_by_left + 1e-3,
            "right edge should follow the lamp right: {right_lit_by_right} vs {right_lit_by_left}"
        );
        let left_lit_by_left = light_gain(lamp_left, Edge::Left, 0.5, 0.65);
        let left_lit_by_right = light_gain(lamp_right, Edge::Left, 0.5, 0.65);
        assert!(
            left_lit_by_left > left_lit_by_right + 1e-3,
            "left edge should follow the lamp left: {left_lit_by_left} vs {left_lit_by_right}"
        );
    }

    #[test]
    fn zero_power_returns_zero_everywhere() {
        let dark = AreaLight {
            power: 0.0,
            ..AREA_LIGHT
        };
        for edge in Edge::ALL {
            for v in [0.0, 0.2, 0.5, 0.8, 1.0] {
                assert_eq!(light_gain(dark, edge, 0.5, v), 0.0);
            }
            let (w, h, rgba) = strip_rgba(dark, edge, 24, 6);
            assert_eq!(rgba.len(), (w * h * 4) as usize);
            assert!(rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0));
        }
    }
}
