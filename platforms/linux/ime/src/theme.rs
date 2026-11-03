//! 候选窗配色。浅色与深色两套令牌与 Windows、macOS 保持一致。
//!
//! Linux 没有统一的外观查询接口;`LUEYING_THEME=dark` 优先,其次看
//! `GTK_THEME` 是否带 Dark。缺省用浅色。

use lueying_render::panel::Palette;

const LIGHT: Palette = Palette {
    bg: 0xffffff,
    window_bg: 0xf3f4f7,
    surface_alt: 0xf7f8fa,
    border: 0xe2e4e9,
    border_strong: 0xcfd2d9,
    text: 0x1b1c1f,
    text_secondary: 0x5f636b,
    text_tertiary: 0x6a6e77,
    accent: 0x2f6fed,
    on_accent: 0xffffff,
    segment: 0xffffff,
    off_bg: 0xd9dce3,
    knob_off: 0xffffff,
    danger: 0xb3261e,
    danger_soft: 0xfdecea,
    accent_text: 0x2459c9,
    selected_bg: 0xeaf1ff,
    selected_text: 0x1b1c1f,
    hover: 0xf0f2f6,
};

const DARK: Palette = Palette {
    bg: 0x202127,
    window_bg: 0x17181c,
    surface_alt: 0x262830,
    border: 0x30323a,
    border_strong: 0x42454f,
    text: 0xecedf2,
    text_secondary: 0xa6aab4,
    text_tertiary: 0x8e929c,
    accent: 0x2f6fed,
    on_accent: 0xffffff,
    segment: 0x343741,
    off_bg: 0x3a3d46,
    knob_off: 0xffffff,
    danger: 0xf28b82,
    danger_soft: 0x3a2424,
    accent_text: 0x7aa7ff,
    selected_bg: 0x2a3b57,
    selected_text: 0xecedf2,
    hover: 0x2a2c34,
};

pub fn current() -> Palette {
    if let Ok(theme) = std::env::var("LUEYING_THEME") {
        return if theme.to_ascii_lowercase().contains("dark") {
            DARK
        } else {
            LIGHT
        };
    }
    if let Ok(theme) = std::env::var("GTK_THEME")
        && theme.to_ascii_lowercase().contains("dark")
    {
        return DARK;
    }
    LIGHT
}
