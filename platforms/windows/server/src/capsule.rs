//! 选中胶囊的灯光材质:候选窗与设置预览共用。
//!
//! 这里刻意不用釉面:胶囊中心保持不透明的主题色,文字对比不被高光
//! 稀释;光只放在轮廓与外溢光里。外溢光垫在实色胶囊后面,即使很强
//! 也不会回到「边缘亮、中心透底」的空心观感。

use lueying_render::panel::{self, Rect};
use rune::{Color, El, el};

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
    // 真光时代不再贴假光晕:黑瓷里胶囊是真的光源,光池由 rune-light
    // 物理辐射;玻璃里它是踏实的实色元件。保留上下棱线只做塑形。
    capsule.border_gradient(stroke, Color::WHITE.alpha(0.34), Color::BLACK.alpha(0.14))
}
