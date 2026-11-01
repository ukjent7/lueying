//! 设置窗与徽标的 Skia 小助手:0xRRGGBB 转 Skia 颜色与填充画刷。
//!
//! 候选窗已换 D2D(`server::ui_d2d_pipe`);这些助手服务剩下的 Skia
//! 自绘面(设置控件、命令面板、徽标),P3 换 Slint 时一起退场。

use skia_safe::{Color, Paint};

/// 0xRRGGBB 转 Skia 颜色。
pub(crate) fn color(rgb: u32) -> Color {
    Color::from_rgb(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

/// 抗锯齿的纯色填充画刷。
pub(crate) fn fill_paint(rgb: u32) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(rgb));
    paint
}
