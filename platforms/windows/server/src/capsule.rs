//! 选中胶囊的灯光材质:候选窗与设置预览共用。
//!
//! 这里刻意不用釉面:胶囊中心保持不透明的主题色,文字对比不被高光
//! 稀释;光只放在轮廓与外溢光里。外溢光垫在实色胶囊后面,即使很强
//! 也不会回到「边缘亮、中心透底」的空心观感。

use lueying_render::panel::{self, Rect};
use rune::{Color, El, Shadow, el};

/// 一个已定位的选中胶囊背景。调用方负责 key、即时形态切换与命中/语义;
/// 这个元素只负责视觉,不暴露辅助功能节点。
pub fn selection_capsule(rect: Rect, scale: f32, fill: Color, high_contrast: bool) -> El {
    let radius = panel::capsule_radius(rect.width(), rect.height());
    let capsule = el()
        .absolute(rect.left, rect.top)
        .w(rect.width())
        .h(rect.height())
        .radius(radius)
        .fill(fill)
        .a11y_hidden();
    if high_contrast {
        return capsule;
    }

    let stroke = scale.max(0.75);
    capsule
        // 上缘接住白光,下缘轻轻压暗:只是照明,不改变中心主题色。
        .border_gradient(stroke, Color::WHITE.alpha(0.34), Color::BLACK.alpha(0.14))
        // 近处一圈较实的色光让胶囊像光源,远处再散开。两层都是 accent,
        // 不引入黄色釉光或独立灯管色。
        .box_shadow(Shadow {
            y: 0.0,
            blur: 5.0 * scale,
            color: fill.alpha(0.28),
            ..Shadow::default()
        })
        .box_shadow(Shadow {
            y: scale,
            blur: 12.0 * scale,
            color: fill.alpha(0.14),
            ..Shadow::default()
        })
}
