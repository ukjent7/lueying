//! 模式徽标的绘制:圆角卡片 + 描边 + 居中的一个字。
//!
//! 徽标是候选窗之外的第二个表面:尺寸固定、只放一个字、中英切换时短暂
//! 出现。平台侧只管建窗与上屏,画面由这里给出,配色与候选窗同源。

use skia_safe::{
    BlurStyle, Canvas, Color, Font, MaskFilter, Paint, PaintStyle, Point, RRect, Rect, Vector,
};

use crate::panel::{Palette, color, fill_paint};

/// 徽标卡片宽度(逻辑像素)。
pub const WIDTH: f32 = 46.0;
/// 徽标卡片高度(逻辑像素)。
pub const HEIGHT: f32 = 32.0;
/// 圆角半径(逻辑像素)。
const RADIUS: f32 = 8.0;
/// 徽标字号(逻辑像素)。
const TEXT_SIZE: f32 = 17.0;

/// 一枚徽标的画法参数。
pub struct BadgeSpec<'a> {
    /// 显示的字:「中」「英」「全」「半」。
    pub label: &'a str,
    /// 配色令牌,与候选窗同一套。
    pub palette: &'a Palette,
    /// 字型;没有时只画卡片,不留空白文字。
    pub font: Option<&'a Font>,
    /// 卡片尺寸(物理像素)。
    pub width: f32,
    pub height: f32,
    /// DPI 缩放。
    pub scale: f32,
    /// 是否画投影;高对比度主题关掉。
    pub shadow: bool,
}

/// 画一枚模式徽标:圆角卡片 + 描边 + 居中的字。
pub fn draw(canvas: &Canvas, badge: &BadgeSpec<'_>) {
    let radius = RADIUS * badge.scale;
    if badge.shadow {
        // 轻投影把徽标从宿主窗口上托起来。
        let mut halo = Paint::default();
        halo.set_anti_alias(true);
        halo.set_color(Color::from_argb(28, 0, 0, 0));
        if let Some(blur) = MaskFilter::blur(BlurStyle::Normal, 6.0 * badge.scale, Some(true)) {
            halo.set_mask_filter(blur);
        }
        canvas.draw_rrect(
            RRect::new_rect_radii(
                Rect::from_xywh(
                    1.0 * badge.scale,
                    2.0 * badge.scale,
                    badge.width - 2.0 * badge.scale,
                    badge.height - 2.0 * badge.scale,
                ),
                &[Vector::new(radius, radius); 4],
            ),
            &halo,
        );
    }
    // 底色与描边。
    let body = Rect::from_xywh(
        0.5 * badge.scale,
        0.5 * badge.scale,
        badge.width - badge.scale,
        badge.height - badge.scale,
    );
    canvas.draw_rrect(
        RRect::new_rect_radii(body, &[Vector::new(radius, radius); 4]),
        &fill_paint(badge.palette.bg),
    );
    let mut stroke = Paint::default();
    stroke.set_anti_alias(true);
    stroke.set_style(PaintStyle::Stroke);
    stroke.set_stroke_width(badge.scale);
    stroke.set_color(color(badge.palette.border));
    canvas.draw_rrect(
        RRect::new_rect_radii(
            body,
            &[Vector::new(radius - 0.5 * badge.scale, radius - 0.5 * badge.scale); 4],
        ),
        &stroke,
    );
    // 字:没有字型就只剩一张卡片,总比空窗强。
    let Some(font) = badge.font else {
        return;
    };
    let mut text_font = font.clone();
    text_font.set_size(TEXT_SIZE * badge.scale);
    let text_width = text_font.measure_str(badge.label, None).0;
    let (_, metrics) = text_font.metrics();
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(badge.palette.text));
    // 基线按字体的上下留白居中,而不是拍一个固定偏移。
    let baseline = badge.height / 2.0 - (metrics.ascent + metrics.descent) / 2.0;
    canvas.draw_str(
        badge.label,
        Point::new((badge.width - text_width) / 2.0, baseline),
        &text_font,
        &paint,
    );
}
