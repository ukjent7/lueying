//! 模式徽标的绘制:圆角卡片 + 描边 + 居中的一个字。
//!
//! 徽标是候选窗之外的第二个表面:尺寸固定、只放一个字、中英切换时短暂
//! 出现。平台侧只管建窗与上屏,画面由这里给出,配色与候选窗同源。

use crate::paint::{Canvas, Color};
use crate::panel::{Palette, Rect};

/// 徽标卡片宽度(逻辑像素)。
pub const WIDTH: f32 = 46.0;
/// 徽标卡片高度(逻辑像素)。
pub const HEIGHT: f32 = 32.0;
/// 圆角半径(逻辑像素)。
const RADIUS: f32 = 8.0;
/// 徽标字号(逻辑像素)。
const TEXT_SIZE: f32 = 17.0;
/// 投影扩散半径(逻辑像素)。
const SHADOW_BLUR: f32 = 6.0;

/// 一枚徽标的画法参数。
pub struct BadgeSpec<'a> {
    /// 显示的字:「中」「英」「全」「半」。
    pub label: &'a str,
    /// 配色令牌,与候选窗同一套。
    pub palette: &'a Palette,
    /// 卡片尺寸(物理像素)。
    pub width: f32,
    pub height: f32,
    /// DPI 缩放。
    pub scale: f32,
    /// 整枚徽标的不透明度(淡入淡出);1 为全不透明。
    pub opacity: f32,
    /// 是否画投影;高对比度主题关掉。
    pub shadow: bool,
}

/// 按整枚徽标的不透明度淡化一种颜色。
fn fade(color: Color, opacity: f32) -> Color {
    let alpha = (f32::from(color.alpha) * opacity.clamp(0.0, 1.0)).round() as u8;
    Color::with_alpha(color.rgb, alpha)
}

/// 画一枚模式徽标:圆角卡片 + 描边 + 居中的字。
pub fn draw(canvas: &mut dyn Canvas, badge: &BadgeSpec<'_>) {
    let radius = RADIUS * badge.scale;
    let body = Rect::from_xywh(
        0.5 * badge.scale,
        0.5 * badge.scale,
        badge.width - badge.scale,
        badge.height - badge.scale,
    );
    if badge.shadow {
        // 轻投影把徽标从宿主窗口上托起来。
        canvas.shadow_rrect(
            body,
            radius,
            SHADOW_BLUR * badge.scale,
            fade(Color::with_alpha(0x000000, 28), badge.opacity),
        );
    }
    canvas.fill_rrect(
        body,
        radius,
        fade(Color::opaque(badge.palette.bg), badge.opacity),
    );
    canvas.stroke_rrect(
        body,
        (radius - 0.5 * badge.scale).max(0.0),
        badge.scale,
        fade(Color::opaque(badge.palette.border), badge.opacity),
    );
    canvas.text_centered(
        badge.label,
        body,
        TEXT_SIZE * badge.scale,
        fade(Color::opaque(badge.palette.text), badge.opacity),
    );
}
