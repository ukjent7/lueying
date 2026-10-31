//! 声明式皮肤:`skin.toml` 覆盖候选窗配色,SkSL 效果目录是它上面的高级层。
//!
//! 文件在 `%APPDATA%\lueying\skin.toml`,浅/深两张表,缺的键用主题默认。
//! 值是 TOML 十六进制整数 `0xRRGGBB`;键名与 `theme::Colors` 字段一一对应。
//! 坏文件回落默认并收集问题(写日志),不阻断输入;高对比度下整份忽略,
//! 由 theme 侧执行——那是读屏与对比度的宿主不变量。
//!
//! 与 SkSL 的分工:皮肤管「颜色」,不产生动态效果、不接触合成;要动效
//! 就写效果文件(`docs/effects.md`)。文字对比度不校验,自定义色配出的
//! 可读性由用户负责,效果遮罩(`panel::scrim_alpha`)只对效果层兜底。

use std::path::Path;

use serde::Deserialize;

/// 解析好的皮肤;`None` 表就是没覆盖,整份空 Skin 等于默认主题。
#[derive(Default, Clone, Copy, Debug)]
pub struct Skin {
    /// 浅色模式的覆盖。
    pub light: Option<SkinColors>,
    /// 深色模式的覆盖。
    pub dark: Option<SkinColors>,
}

/// 一张表里可覆盖的颜色;字段与 `theme::Colors` 一一对应。
#[derive(Deserialize, Default, Clone, Copy, Debug)]
#[serde(deny_unknown_fields)]
pub struct SkinColors {
    pub bg: Option<u32>,
    pub surface: Option<u32>,
    pub surface_alt: Option<u32>,
    pub segment: Option<u32>,
    pub border: Option<u32>,
    pub border_strong: Option<u32>,
    pub text: Option<u32>,
    pub text_secondary: Option<u32>,
    pub text_tertiary: Option<u32>,
    pub accent: Option<u32>,
    pub accent_hover: Option<u32>,
    pub accent_text: Option<u32>,
    pub on_accent: Option<u32>,
    pub hover: Option<u32>,
    pub off_bg: Option<u32>,
    pub knob_off: Option<u32>,
    pub success: Option<u32>,
    pub danger: Option<u32>,
    pub danger_soft: Option<u32>,
    pub selected_bg: Option<u32>,
    pub selected_text: Option<u32>,
}

impl SkinColors {
    /// 是否一个键都没覆盖。
    pub fn is_empty(&self) -> bool {
        self.bg.is_none()
            && self.surface.is_none()
            && self.surface_alt.is_none()
            && self.segment.is_none()
            && self.border.is_none()
            && self.border_strong.is_none()
            && self.text.is_none()
            && self.text_secondary.is_none()
            && self.text_tertiary.is_none()
            && self.accent.is_none()
            && self.accent_hover.is_none()
            && self.accent_text.is_none()
            && self.on_accent.is_none()
            && self.hover.is_none()
            && self.off_bg.is_none()
            && self.knob_off.is_none()
            && self.success.is_none()
            && self.danger.is_none()
            && self.danger_soft.is_none()
            && self.selected_bg.is_none()
            && self.selected_text.is_none()
    }
}

/// 皮肤文件的 TOML schema。
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct SkinFile {
    light: Option<SkinColors>,
    dark: Option<SkinColors>,
}

impl Skin {
    /// 读皮肤文件;文件缺失是常态(默认主题),解析失败收集成问题。
    pub fn load(path: &Path) -> (Skin, Vec<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => return (Skin::default(), Vec::new()),
        };
        let file: SkinFile = match toml::from_str(&text) {
            Ok(file) => file,
            Err(e) => return (Skin::default(), vec![format!("skin.toml 解析失败: {e}")]),
        };
        let mut issues = Vec::new();
        let light = file.light.and_then(|c| clean(c, "light", &mut issues));
        let dark = file.dark.and_then(|c| clean(c, "dark", &mut issues));
        (Skin { light, dark }, issues)
    }

    /// 是否整份都没覆盖。
    pub fn is_empty(&self) -> bool {
        self.light.is_none() && self.dark.is_none()
    }

    /// 对应模式的覆盖表;`dark` 为 true 取深色那张。
    pub fn for_appearance(&self, dark: bool) -> Option<&SkinColors> {
        if dark {
            self.dark.as_ref()
        } else {
            self.light.as_ref()
        }
    }
}

/// 值域检查:0xRRGGBB 之外的值按坏值丢掉并记问题,其余键照常生效。
fn clean(raw: SkinColors, table: &str, issues: &mut Vec<String>) -> Option<SkinColors> {
    let mut checked = raw;
    for (name, value) in [
        ("bg", &mut checked.bg),
        ("surface", &mut checked.surface),
        ("surface_alt", &mut checked.surface_alt),
        ("segment", &mut checked.segment),
        ("border", &mut checked.border),
        ("border_strong", &mut checked.border_strong),
        ("text", &mut checked.text),
        ("text_secondary", &mut checked.text_secondary),
        ("text_tertiary", &mut checked.text_tertiary),
        ("accent", &mut checked.accent),
        ("accent_hover", &mut checked.accent_hover),
        ("accent_text", &mut checked.accent_text),
        ("on_accent", &mut checked.on_accent),
        ("hover", &mut checked.hover),
        ("off_bg", &mut checked.off_bg),
        ("knob_off", &mut checked.knob_off),
        ("success", &mut checked.success),
        ("danger", &mut checked.danger),
        ("danger_soft", &mut checked.danger_soft),
        ("selected_bg", &mut checked.selected_bg),
        ("selected_text", &mut checked.selected_text),
    ] {
        if let Some(rgb) = *value
            && rgb > 0xff_ff_ff
        {
            issues.push(format!(
                "skin.toml [{table}] {name}=0x{rgb:x} 不是 0xRRGGBB,已忽略"
            ));
            *value = None;
        }
    }
    if checked.is_empty() {
        return None;
    }
    Some(checked)
}

#[cfg(test)]
#[path = "skin_tests.rs"]
mod tests;
