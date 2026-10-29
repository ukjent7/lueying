//! 系统主题查询与共享设计令牌。
//! 候选窗、设置窗与模式徽标共用同一套颜色,避免三处各写一份后走样。

use std::ffi::c_void;

use gpui::{Window, WindowAppearance};
use lueying_render::panel::Palette;
use windows::Win32::Graphics::Gdi::{
    COLOR_3DFACE, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
    COLOR_WINDOWFRAME, COLOR_WINDOWTEXT, GetSysColor, SYS_COLOR_INDEX,
};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

/// 当前窗口使用的外观。高对比度优先于深浅色。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
    HighContrast,
}

impl Appearance {
    pub fn of(window: &Window) -> Self {
        if high_contrast() {
            Self::HighContrast
        } else if matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ) {
            Self::Dark
        } else {
            Self::Light
        }
    }

    /// 无 gpui 窗口的路径(Skia 候选窗)用:注册表读系统浅色/深色,
    /// 读不到按浅色。高对比度的判定与 `of` 相同。
    pub fn of_system() -> Self {
        if high_contrast() {
            Self::HighContrast
        } else if apps_use_light() {
            Self::Light
        } else {
            Self::Dark
        }
    }
}

/// `AppsUseLightTheme`:0 表示系统应用走深色;键或值读不到时按浅色处理。
fn apps_use_light() -> bool {
    windows_registry::CURRENT_USER
        .open("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
        .and_then(|key| key.get_u32("AppsUseLightTheme"))
        .map_or(true, |value| value != 0)
}

/// 候选窗主题色预设。`light`/`dark` 各是 [accent, accent_hover,
/// accent_text, selected_bg];高对比度主题始终用系统色,不吃预设。
pub struct Accent {
    pub name: &'static str,
    pub light: [u32; 4],
    pub dark: [u32; 4],
}

/// 首位是出厂蓝;新增预设加在表尾,旧配置里的下标不变。
pub const ACCENTS: [Accent; 6] = [
    Accent {
        name: "蓝",
        light: [0x2f6fed, 0x255fd0, 0x2459c9, 0xeaf1ff],
        dark: [0x2f6fed, 0x255fd0, 0x7aa7ff, 0x2a3b57],
    },
    Accent {
        name: "青",
        light: [0x0d7377, 0x0a5f63, 0x095457, 0xe3f2f2],
        dark: [0x1fb6bd, 0x1aa0a6, 0x6fd8dd, 0x1c3f42],
    },
    Accent {
        name: "绿",
        light: [0x167a34, 0x116329, 0x0e5526, 0xe5f3ea],
        dark: [0x2fae5e, 0x279750, 0x83d3a1, 0x1c3a27],
    },
    Accent {
        name: "紫",
        light: [0x6b4fd8, 0x5a41c4, 0x4f38b0, 0xefecfc],
        dark: [0x8f78ee, 0x7d65e2, 0xbcaafb, 0x2c2547],
    },
    Accent {
        name: "红",
        light: [0xb4342c, 0x9c2a24, 0x8c251f, 0xf9e9e8],
        dark: [0xd75f57, 0xc74e46, 0xf0a09a, 0x452320],
    },
    Accent {
        name: "橙",
        light: [0xb25e12, 0x994f0d, 0x8a480c, 0xf9efe2],
        dark: [0xd9862e, 0xc37522, 0xf0b266, 0x44301b],
    },
];

/// 主题色配置值转下标;接受颜色名或下标,越界回出厂蓝。
pub fn accent_index(value: &str) -> Option<usize> {
    if let Ok(index) = value.parse::<usize>() {
        return (index < ACCENTS.len()).then_some(index);
    }
    ACCENTS.iter().position(|a| a.name == value)
}

/// 按主题色预设取一套界面颜色;下标越界或高对比度时回出厂行为。
pub fn colors_with(appearance: Appearance, accent: usize) -> Colors {
    let mut colors = colors(appearance);
    let preset = match appearance {
        Appearance::HighContrast => return colors,
        Appearance::Light => ACCENTS.get(accent).map(|a| a.light),
        Appearance::Dark => ACCENTS.get(accent).map(|a| a.dark),
    };
    if let Some([accent, accent_hover, accent_text, selected_bg]) = preset {
        colors.accent = accent;
        colors.accent_hover = accent_hover;
        colors.accent_text = accent_text;
        colors.selected_bg = selected_bg;
    }
    colors
}

/// 一套界面颜色。字段按用途命名,数值均为 0xRRGGBB。
#[derive(Clone, Copy)]
pub struct Colors {
    /// 窗口底色。
    pub bg: u32,
    /// 卡片、候选窗底色。
    pub surface: u32,
    /// 预编辑条、分段控件轨道等次级表面。
    pub surface_alt: u32,
    /// 分段控件选中的那一格;浅色下浮起,深色下比轨道亮一档。
    pub segment: u32,
    pub border: u32,
    /// 输入框、次级按钮的描边。
    pub border_strong: u32,
    pub text: u32,
    pub text_secondary: u32,
    pub text_tertiary: u32,
    /// 主色填充;与白字搭配时保持 4.5:1。
    pub accent: u32,
    pub accent_hover: u32,
    /// 深色底上的主色文字/图标。
    pub accent_text: u32,
    pub on_accent: u32,
    pub hover: u32,
    /// 开关关闭时的轨道。
    pub off_bg: u32,
    /// 开关关闭时的圆点。
    pub knob_off: u32,
    pub success: u32,
    pub danger: u32,
    pub danger_soft: u32,
    pub selected_bg: u32,
    pub selected_text: u32,
}

pub fn colors(appearance: Appearance) -> Colors {
    match appearance {
        Appearance::Light => Colors {
            bg: 0xf3f4f7,
            surface: 0xffffff,
            surface_alt: 0xf7f8fa,
            segment: 0xffffff,
            border: 0xe2e4e9,
            border_strong: 0xcfd2d9,
            text: 0x1b1c1f,
            text_secondary: 0x5f636b,
            text_tertiary: 0x6a6e77,
            accent: 0x2f6fed,
            accent_hover: 0x255fd0,
            accent_text: 0x2459c9,
            on_accent: 0xffffff,
            hover: 0xf0f2f6,
            off_bg: 0xd9dce3,
            knob_off: 0xffffff,
            success: 0x167a34,
            danger: 0xb3261e,
            danger_soft: 0xfdecea,
            selected_bg: 0xeaf1ff,
            selected_text: 0x1b1c1f,
        },
        Appearance::Dark => Colors {
            bg: 0x17181c,
            surface: 0x202127,
            surface_alt: 0x262830,
            segment: 0x343741,
            border: 0x30323a,
            border_strong: 0x42454f,
            text: 0xecedf2,
            text_secondary: 0xa6aab4,
            text_tertiary: 0x8e929c,
            accent: 0x2f6fed,
            accent_hover: 0x255fd0,
            accent_text: 0x7aa7ff,
            on_accent: 0xffffff,
            hover: 0x2a2c34,
            off_bg: 0x3a3d46,
            knob_off: 0xffffff,
            success: 0x81c995,
            danger: 0xf28b82,
            danger_soft: 0x3a2424,
            selected_bg: 0x2a3b57,
            selected_text: 0xecedf2,
        },
        Appearance::HighContrast => {
            let text = system_color(COLOR_WINDOWTEXT);
            let highlight = system_color(COLOR_HIGHLIGHT);
            let on_highlight = system_color(COLOR_HIGHLIGHTTEXT);
            Colors {
                bg: system_color(COLOR_WINDOW),
                surface: system_color(COLOR_WINDOW),
                surface_alt: system_color(COLOR_WINDOW),
                segment: highlight,
                border: system_color(COLOR_WINDOWFRAME),
                border_strong: text,
                text,
                text_secondary: system_color(COLOR_GRAYTEXT),
                text_tertiary: system_color(COLOR_GRAYTEXT),
                accent: highlight,
                accent_hover: highlight,
                accent_text: on_highlight,
                on_accent: on_highlight,
                hover: system_color(COLOR_3DFACE),
                off_bg: system_color(COLOR_3DFACE),
                knob_off: text,
                success: text,
                danger: text,
                danger_soft: system_color(COLOR_WINDOW),
                selected_bg: highlight,
                selected_text: on_highlight,
            }
        }
    }
}

/// 系统是否处于高对比度主题。查询失败按普通主题处理。
pub fn high_contrast() -> bool {
    let mut info = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            info.cbSize,
            Some((&mut info as *mut HIGHCONTRASTW).cast::<c_void>()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok()
        && info.dwFlags.contains(HCF_HIGHCONTRASTON)
}

/// COLORREF(0x00BBGGRR)→ GPUI 的 0xRRGGBB。
pub fn system_color(index: SYS_COLOR_INDEX) -> u32 {
    let color = unsafe { GetSysColor(index) };
    ((color & 0xff) << 16) | (color & 0xff00) | ((color >> 16) & 0xff)
}

/// 界面色 → 渲染令牌;候选窗与模式徽标共用同一套。
///
/// 渲染层的 `bg` 是面板底色(浅色下的白、深色下的深灰),设置窗的底色
/// 是另一张牌,不走渲染层。
pub fn palette(colors: Colors) -> Palette {
    Palette {
        bg: colors.surface,
        surface_alt: colors.surface_alt,
        border: colors.border,
        text: colors.text,
        text_secondary: colors.text_secondary,
        text_tertiary: colors.text_tertiary,
        accent: colors.accent,
        accent_text: colors.accent_text,
        selected_bg: colors.selected_bg,
        selected_text: colors.selected_text,
        hover: colors.hover,
    }
}
