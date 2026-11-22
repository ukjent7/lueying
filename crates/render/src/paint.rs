//! 绘制端口:面板只描述画什么,平台侧决定怎么画。
//!
//! 候选窗走 Direct2D 设备上下文,徽标与面板走 Direct2D 兼容的 DC 表面;
//! Linux E2E 用记录画布断言绘制顺序与几何。三个后端的坐标都是物理像素,
//! 由调用方按 DPI 缩放后传入。

use crate::panel::Rect;

/// 0xRRGGBB 加 0–255 透明度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub rgb: u32,
    pub alpha: u8,
}

impl Color {
    pub const fn opaque(rgb: u32) -> Self {
        Self { rgb, alpha: 255 }
    }

    pub const fn with_alpha(rgb: u32, alpha: u8) -> Self {
        Self { rgb, alpha }
    }
}

/// 文字锚点:左缘与竖直中线(物理像素)。
///
/// 用中线而非基线:后端只要把行盒竖直居中在 `y`,字形位置就与字型
/// 自带的 ascent/descent 无关,Direct2D 与记录画布量出的结果一致。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 面板绘制接口。所有方法都在一块透明画布上操作,窗口层负责清屏与上屏。
pub trait Canvas {
    /// 纯色矩形。
    fn fill_rect(&mut self, rect: Rect, color: Color);

    /// 矩形描边,`width` 是线宽(物理像素),画在矩形内侧。
    fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color);

    /// 圆角矩形,`radius` 是圆角半径(物理像素)。
    fn fill_rrect(&mut self, rect: Rect, radius: f32, color: Color);

    /// 竖向渐变的圆角矩形;选中块的亮面釉靠它做上亮下深。
    ///
    /// 默认退化成中间色实心,平台没有渐变笔时形状与不透明度仍一致;
    /// 有渐变笔的后端覆盖它,亮面质感才完整。
    fn fill_rrect_vertical(&mut self, rect: Rect, radius: f32, top: Color, bottom: Color) {
        let mix = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
        let channel = |shift: u32| {
            mix(
                ((top.rgb >> shift) & 0xff) as u8,
                ((bottom.rgb >> shift) & 0xff) as u8,
            )
        };
        let rgb =
            (u32::from(channel(16)) << 16) | (u32::from(channel(8)) << 8) | u32::from(channel(0));
        self.fill_rrect(
            rect,
            radius,
            Color::with_alpha(rgb, mix(top.alpha, bottom.alpha)),
        );
    }

    /// Optional elliptical light, clipped to the rounded surface. The base is painted separately.
    fn light_rrect(
        &mut self,
        _rect: Rect,
        _radius: f32,
        _center: Point,
        _extent: Point,
        _stops: &[(f32, Color)],
    ) {
    }

    /// 圆角矩形描边。
    fn stroke_rrect(&mut self, rect: Rect, radius: f32, width: f32, color: Color);

    /// 圆角矩形外的一圈柔和投影;`blur` 是扩散半径。
    fn shadow_rrect(&mut self, rect: Rect, radius: f32, blur: f32, color: Color);

    /// 线段;画矢量图形(如条尾箭头),`width` 是线宽(物理像素)。
    fn line(&mut self, a: Point, b: Point, width: f32, color: Color);

    /// 以左缘与竖直中线为锚点画一段文字;`max_width` 给定时裁掉超出部分。
    fn text(
        &mut self,
        content: &str,
        origin: Point,
        size: f32,
        color: Color,
        max_width: Option<f32>,
    );

    /// 一段文字的绘制宽度(物理像素)。
    fn measure(&mut self, content: &str, size: f32) -> f32;

    /// 在矩形内水平、垂直居中画一段文字。
    fn text_centered(&mut self, content: &str, rect: Rect, size: f32, color: Color);
}
