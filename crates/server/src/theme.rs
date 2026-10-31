//! 系统主题查询与共享设计令牌。
//! 候选窗、设置窗与模式徽标共用同一套颜色,避免三处各写一份后走样。

use std::ffi::c_void;

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
    /// 读系统外观:注册表读浅色/深色,读不到按浅色。
    /// 高对比度的判定与系统设置一致。
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

/// 候选窗主题色预设在 `accents` 里;这里只转出,免得调用方改 import。
pub use crate::accents::{ACCENTS, Accent, accent_index};

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

/// 运行期脚本给的主题色:换主色与选中底色,文字色沿用系统主题。
///
/// 选中文字与弱文字都还是主题里那套颜色,脚本换一个再亮的主题色,
/// 对比度也有下限;高对比度下整条忽略。
pub fn colors_with_accent(appearance: Appearance, accent: u32) -> Colors {
    let mut colors = colors(appearance);
    if matches!(appearance, Appearance::HighContrast) {
        return colors;
    }
    colors.accent = accent;
    colors.accent_hover = accent;
    let weight = match appearance {
        Appearance::Light => 0.08,
        _ => 0.20,
    };
    colors.selected_bg = blend(colors.surface, accent, weight);
    colors
}

/// 声明式皮肤覆盖:skin.toml 的浅/深表按字段盖到颜色上,缺的键保留
/// 主题值。高对比度整份忽略——对比度与读屏是宿主不变量,自定义配色
/// 不能压过它;脚本主题色同理,皮肤在其之上,是用户显式放置的最后一层。
pub fn with_skin(colors: Colors, appearance: Appearance, skin: &crate::skin::Skin) -> Colors {
    if matches!(appearance, Appearance::HighContrast) {
        return colors;
    }
    let Some(over) = skin.for_appearance(matches!(appearance, Appearance::Dark)) else {
        return colors;
    };
    let mut c = colors;
    if let Some(v) = over.bg {
        c.bg = v;
    }
    if let Some(v) = over.surface {
        c.surface = v;
    }
    if let Some(v) = over.surface_alt {
        c.surface_alt = v;
    }
    if let Some(v) = over.segment {
        c.segment = v;
    }
    if let Some(v) = over.border {
        c.border = v;
    }
    if let Some(v) = over.border_strong {
        c.border_strong = v;
    }
    if let Some(v) = over.text {
        c.text = v;
    }
    if let Some(v) = over.text_secondary {
        c.text_secondary = v;
    }
    if let Some(v) = over.text_tertiary {
        c.text_tertiary = v;
    }
    if let Some(v) = over.accent {
        c.accent = v;
    }
    if let Some(v) = over.accent_hover {
        c.accent_hover = v;
    }
    if let Some(v) = over.accent_text {
        c.accent_text = v;
    }
    if let Some(v) = over.on_accent {
        c.on_accent = v;
    }
    if let Some(v) = over.hover {
        c.hover = v;
    }
    if let Some(v) = over.off_bg {
        c.off_bg = v;
    }
    if let Some(v) = over.knob_off {
        c.knob_off = v;
    }
    if let Some(v) = over.success {
        c.success = v;
    }
    if let Some(v) = over.danger {
        c.danger = v;
    }
    if let Some(v) = over.danger_soft {
        c.danger_soft = v;
    }
    if let Some(v) = over.selected_bg {
        c.selected_bg = v;
    }
    if let Some(v) = over.selected_text {
        c.selected_text = v;
    }
    c
}

/// 按权重把 `tint` 混进 `base`;0 保留底色,1 取满主题色。
fn blend(base: u32, tint: u32, weight: f32) -> u32 {
    let mix = |shift: u32| {
        let base = ((base >> shift) & 0xff) as f32;
        let tint = ((tint >> shift) & 0xff) as f32;
        (base + (tint - base) * weight).round() as u32
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
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
        window_bg: colors.bg,
        surface_alt: colors.surface_alt,
        border: colors.border,
        border_strong: colors.border_strong,
        text: colors.text,
        text_secondary: colors.text_secondary,
        text_tertiary: colors.text_tertiary,
        accent: colors.accent,
        on_accent: colors.on_accent,
        segment: colors.segment,
        off_bg: colors.off_bg,
        knob_off: colors.knob_off,
        danger: colors.danger,
        danger_soft: colors.danger_soft,
        accent_text: colors.accent_text,
        selected_bg: colors.selected_bg,
        selected_text: colors.selected_text,
        hover: colors.hover,
    }
}
