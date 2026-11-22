//! 系统主题查询与共享设计令牌。
//! 候选窗、设置窗与模式徽标共用同一套颜色,避免三处各写一份后走样。

use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use lueying_render::panel::Palette;
use rune::Color;
use rune::color::srgb_decode;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Gdi::{
    COLOR_3DFACE, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW,
    COLOR_WINDOWFRAME, COLOR_WINDOWTEXT, GetSysColor, SYS_COLOR_INDEX,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW,
    RegQueryValueExW,
};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW,
};
use windows::core::{BOOL, HSTRING, PCWSTR};

/// 客户区动画的系统开关(设置 → 辅助功能 → 视觉效果 → 动画效果)。
/// 关掉时 Rune 把弹簧动效降级为淡出;进程内缓存,改系统设置后重启生效。
pub fn client_area_animation() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let mut on = BOOL(0);
        unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                Some((&mut on as *mut BOOL).cast::<c_void>()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .is_ok()
            && on.as_bool()
    })
}

/// 0xRRGGBB → Rune 颜色(线性 sRGB)。候选窗、模式徽标与设置窗共用一份转换。
pub fn srgb(v: u32) -> Color {
    let channel = |shift: u32| srgb_decode(((v >> shift) & 0xff) as f32 / 255.0);
    Color {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: 1.0,
    }
}

/// 0xRRGGBB + 线性透明度。
pub fn srgb_alpha(v: u32, a: f32) -> Color {
    let mut color = srgb(v);
    color.a = a;
    color
}

/// 瓷釉的光:每一点光都有来源——上缘反射元素上方的环境色(加亮),
/// 下缘是被环境压出的釉根(压暗)。候选窗的环境是面板上下实采的桌面
/// 像素,设置窗的环境是自己的窗底;环境暗则釉暗,没有凭空的光。
#[derive(Clone, Copy, PartialEq)]
pub struct Glaze {
    /// 顶部光泽渐变的起点,到面板半高渐隐为同色透明。
    pub sheen_top: Color,
    /// 上缘 1px 釉线:胎的圆缘接住环境光。
    pub edge_top: Color,
    /// 下缘 1px 釉根:贴着环境的遮蔽。
    pub edge_bottom: Color,
}

/// 由环境色算釉光。加亮倍数是釉面的反射率,压暗倍数是遮蔽率。
pub fn glaze_from_ambient(above: Color, below: Color) -> Glaze {
    let tint = |mut color: Color, factor: f32, a: f32| {
        color.r = (color.r * factor).min(1.0);
        color.g = (color.g * factor).min(1.0);
        color.b = (color.b * factor).min(1.0);
        color.a = a;
        color
    };
    Glaze {
        sheen_top: tint(above, 1.4, 0.10),
        edge_top: tint(above, 1.9, 0.9),
        edge_bottom: tint(below, 0.22, 0.8),
    }
}

/// 发光胶囊的灯色:选中胶囊自己是面板的光源。灯芯上白热下饱和,
/// 亮芯上的字墨是灯色的深色同源——光越亮,字越深,同出一源。
#[derive(Clone, Copy, PartialEq)]
pub struct Glow {
    /// 灯芯上端:accent 混白的白热。
    pub core_top: Color,
    /// 灯芯下端:饱和 accent。
    pub core_bottom: Color,
    /// 亮芯上的字墨:accent 混黑的深色同源。
    pub ink: Color,
}

/// 由 accent 算灯色;混白混黑与调色板同一空间(sRGB)。
pub fn glow_of(accent: u32) -> Glow {
    Glow {
        core_top: srgb(blend(accent, 0xffffff, 0.5)),
        core_bottom: srgb(accent),
        ink: srgb(blend(accent, 0x000000, 0.8)),
    }
}

/// 瓷的固定暖黑配色:候选窗与设置窗共用;琥珀釉是瓷的身份色,
/// 不随主题色预设走。高对比度让位系统色,调用方负责分流。
pub fn ceramic_colors() -> Colors {
    Colors {
        bg: 0x262220,
        surface: 0x2e2a24,
        surface_alt: 0x352f27,
        segment: 0x3e362c,
        border: 0x4a4238,
        border_strong: 0x554c3e,
        text: 0xece7df,
        text_secondary: 0xe3ddd2,
        text_tertiary: 0xd4cec3,
        accent: 0xd9a92e,
        accent_hover: 0xc49726,
        accent_text: 0x7a5608,
        on_accent: 0x2b2110,
        hover: 0x3a332a,
        off_bg: 0x443c30,
        knob_off: 0xffffff,
        success: 0x81c995,
        danger: 0xf28b82,
        danger_soft: 0x3a2424,
        selected_bg: 0xfac738,
        selected_text: 0x2b2110,
    }
}

/// 当前窗口使用的外观。高对比度优先于深浅色。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
    HighContrast,
}

impl Appearance {
    /// 读系统外观:注册表读浅色/深色,读不到按浅色。
    /// 高对比度的判定与系统设置一致。候选窗与设置窗每帧都查,
    /// 1 秒内复用上次结果,免掉每帧的注册表与系统调用。
    pub fn of_system() -> Self {
        static CACHE: Mutex<Option<(Instant, Appearance)>> = Mutex::new(None);
        let mut cached = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, appearance)) = *cached
            && at.elapsed() < Duration::from_secs(1)
        {
            return appearance;
        }
        let appearance = if high_contrast() {
            Self::HighContrast
        } else if apps_use_light() {
            Self::Light
        } else {
            Self::Dark
        };
        *cached = Some((Instant::now(), appearance));
        appearance
    }
}

/// `AppsUseLightTheme`:0 表示系统应用走深色;键或值读不到时按浅色处理。
fn apps_use_light() -> bool {
    let path = HSTRING::from("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let mut key = HKEY::default();
    if unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            Some(0),
            KEY_READ,
            &mut key,
        )
    } != ERROR_SUCCESS
    {
        return true;
    }
    let name = HSTRING::from("AppsUseLightTheme");
    let mut kind = REG_VALUE_TYPE::default();
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let queried = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        )
    };
    let _ = unsafe { RegCloseKey(key) };
    queried == ERROR_SUCCESS && kind == REG_DWORD && data != 0
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

/// 运行期扩展给的主题色:换主色与选中底色,文字色沿用系统主题。
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

/// 按权重把 `tint` 混进 `base`;0 保留底色,1 取满主题色。
/// 工具栏月亮的悬停合成与主题派生共用。
pub fn blend(base: u32, tint: u32, weight: f32) -> u32 {
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

/// COLORREF(0x00BBGGRR)→ 主题令牌的 0xRRGGBB。
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
