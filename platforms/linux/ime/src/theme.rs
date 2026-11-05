//! 候选窗配色。浅色与深色令牌取自 `lueying-render`,Linux 与 macOS 共用。
//!
//! Linux 没有统一的外观查询接口;`LUEYING_THEME=dark` 优先,其次看
//! `GTK_THEME` 是否带 Dark。缺省用浅色。

use lueying_render::panel::Palette;

pub fn current() -> Palette {
    if let Ok(theme) = std::env::var("LUEYING_THEME") {
        return if theme.to_ascii_lowercase().contains("dark") {
            Palette::DARK
        } else {
            Palette::LIGHT
        };
    }
    if let Ok(theme) = std::env::var("GTK_THEME")
        && theme.to_ascii_lowercase().contains("dark")
    {
        return Palette::DARK;
    }
    Palette::LIGHT
}
