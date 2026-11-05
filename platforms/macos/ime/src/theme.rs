//! 候选窗的 macOS 配色。浅色与深色令牌取自 `lueying-render`,与 Linux 共用。

use lueying_render::panel::Palette;
use objc2_app_kit::NSAppearance;

/// 跟随系统当前绘制外观;名称里带 Dark 即深色。
pub fn current() -> Palette {
    let name = NSAppearance::currentDrawingAppearance().name().to_string();
    if name.contains("Dark") {
        Palette::DARK
    } else {
        Palette::LIGHT
    }
}
