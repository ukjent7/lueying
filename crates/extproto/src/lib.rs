//! 扩展宿主与 Wasm guest 共用的协议类型。
//!
//! 这个 crate 同时编进宿主与 `wasm32-unknown-unknown`,所以只放数据契约,
//! 不放 wasmi、Windows 或任何宿主侧实现。场景协议仍然只描述面板状态:
//! 没有候选文字、没有宿主应用名、没有击键历史。

#![deny(missing_docs)]

use serde::{Deserialize, Serialize};

/// 场景协议版本。guest 在清单里不声明版本,宿主与 SDK 同步升级。
pub const SCENE_API: u32 = 1;

/// 单个 SkSL 槽位的源码上限。
pub const MAX_EFFECT_BYTES: usize = 64 * 1024;

/// 补丁 JSON 的总大小上限。
pub const MAX_PATCH_BYTES: usize = 3 * MAX_EFFECT_BYTES + 8 * 1024;

/// guest 结果缓冲区的字节数。宿主把服务结果写进这块内存,guest 解析后
/// 再通过 `emit` 交回最终结果。
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;

/// 输入法交给 guest 的面板状态。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SceneIn {
    /// 场景协议版本。
    pub api: u32,
    /// 是否正在组合;false 时面板可能正在淡出。
    pub composing: bool,
    /// 当前页,从 1 开始。
    pub page: u32,
    /// 总页数,至少 1。
    pub pages: u32,
    /// 当前页候选数量,不含序号与文字。
    pub candidates: u32,
    /// 当前页选中候选的下标,从 0 开始。
    pub selected: u32,
    /// 主题色 0xRRGGBB。
    pub accent: u32,
    /// 是否深色主题。
    pub dark: bool,
    /// 系统是否开启高对比度。
    pub high_contrast: bool,
    /// 系统是否开启「减少动态效果」。
    pub reduce_motion: bool,
    /// 动画时钟(秒);减少动态效果时为 0。
    pub time: f32,
    /// 入场/翻页进度 0..1,静止为 1。
    pub anim: f32,
    /// 是否密码/隐私字段。为 true 时 guest 不会被调用。
    pub private: bool,
}

impl Default for SceneIn {
    fn default() -> Self {
        Self {
            api: SCENE_API,
            composing: false,
            page: 1,
            pages: 1,
            candidates: 0,
            selected: 0,
            accent: 0,
            dark: false,
            high_contrast: false,
            reduce_motion: false,
            time: 0.0,
            anim: 1.0,
            private: false,
        }
    }
}

impl SceneIn {
    /// 规整取值:页数至少 1、页码落在页内、选中下标落在候选内、主题色
    /// 截到 24 位、时钟与动画进度是有限值。序列化前统一走这里。
    pub fn cleaned(&self) -> Self {
        let mut state = self.clone();
        state.api = SCENE_API;
        state.pages = state.pages.max(1);
        state.page = state.page.clamp(1, state.pages);
        state.selected = if state.candidates == 0 {
            0
        } else {
            state.selected.min(state.candidates - 1)
        };
        state.accent &= 0x00ff_ffff;
        state.time = if state.time.is_finite() {
            state.time.max(0.0)
        } else {
            0.0
        };
        state.anim = if state.anim.is_finite() {
            state.anim.clamp(0.0, 1.0)
        } else {
            1.0
        };
        state
    }

    /// 规整后序列化成 JSON;非有限浮点不会漏进 guest。
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(&self.cleaned()).map_err(|e| format!("场景序列化失败: {e}"))
    }

    /// 解析宿主传来的场景 JSON;guest 侧用,取值统一走 [`SceneIn::cleaned`]。
    pub fn parse(text: &str) -> Result<Self, String> {
        let scene: Self = serde_json::from_str(text).map_err(|e| format!("场景解析失败: {e}"))?;
        Ok(scene.cleaned())
    }
}

/// guest 返回的补丁;字段全部可选,缺省表示这一项不动。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScenePatch {
    /// 主题色 0xRRGGBB。
    #[serde(default)]
    pub accent: Option<u32>,
    /// 三个 SkSL 槽位的源码。
    #[serde(default)]
    pub effects: Option<EffectsPatch>,
}

impl ScenePatch {
    /// 解析并校验补丁。未知字段、非法主题色、超长或空的源码都拒绝。
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_PATCH_BYTES {
            return Err(format!(
                "补丁超过 {MAX_PATCH_BYTES} 字节(实际 {})",
                text.len()
            ));
        }
        let patch: Self = serde_json::from_str(text).map_err(|e| format!("补丁解析失败: {e}"))?;
        patch.validate()?;
        Ok(patch)
    }

    /// 校验字段取值。
    pub fn validate(&self) -> Result<(), String> {
        if let Some(accent) = self.accent
            && accent > 0x00ff_ffff
        {
            return Err(format!("主题色 0x{accent:06x} 超出 24 位"));
        }
        if let Some(effects) = &self.effects {
            for (slot, source) in effects.iter() {
                if source.trim().is_empty() {
                    return Err(format!("{} 的效果源码为空", slot.title()));
                }
                if source.len() > MAX_EFFECT_BYTES {
                    return Err(format!(
                        "{} 的效果源码超过 {MAX_EFFECT_BYTES} 字节",
                        slot.title()
                    ));
                }
                if source.contains('\0') {
                    return Err(format!("{} 的效果源码含 NUL", slot.title()));
                }
            }
        }
        Ok(())
    }

    /// 按配置顺序合并后一个扩展的补丁:后出现的非空字段覆盖前一个。
    pub fn merge(&mut self, patch: Self) {
        if patch.accent.is_some() {
            self.accent = patch.accent;
        }
        match (self.effects.as_mut(), patch.effects) {
            (Some(current), Some(next)) => current.merge(next),
            (None, Some(next)) => self.effects = Some(next),
            _ => {}
        }
    }
}

/// 三个 SkSL 槽位,与 `lueying-render` 的效果槽位一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectSlot {
    /// 面板底。
    Background,
    /// 选中候选的高亮块。
    Highlight,
    /// 右键菜单底。
    Menu,
}

impl EffectSlot {
    /// 全部槽位;按这个顺序遍历补丁。
    pub const ALL: [Self; 3] = [Self::Background, Self::Highlight, Self::Menu];

    /// 槽位的字段名,与补丁 JSON 的键一致。
    pub fn key(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Highlight => "highlight",
            Self::Menu => "menu",
        }
    }

    /// 槽位的中文名;日志与设置页用。
    pub fn title(self) -> &'static str {
        match self {
            Self::Background => "面板底",
            Self::Highlight => "选中块",
            Self::Menu => "菜单底",
        }
    }
}

/// 三个槽位的 SkSL 源码;缺省的槽位保持当前效果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EffectsPatch {
    /// 面板底。
    #[serde(default)]
    pub background: Option<String>,
    /// 选中块。
    #[serde(default)]
    pub highlight: Option<String>,
    /// 菜单底。
    #[serde(default)]
    pub menu: Option<String>,
}

impl EffectsPatch {
    /// 取某槽位的源码。
    pub fn get(&self, slot: EffectSlot) -> Option<&str> {
        match slot {
            EffectSlot::Background => self.background.as_deref(),
            EffectSlot::Highlight => self.highlight.as_deref(),
            EffectSlot::Menu => self.menu.as_deref(),
        }
    }

    /// 遍历补丁里出现的槽位。
    pub fn iter(&self) -> impl Iterator<Item = (EffectSlot, &str)> + '_ {
        EffectSlot::ALL
            .into_iter()
            .filter_map(|slot| self.get(slot).map(|source| (slot, source)))
    }

    /// 逐槽位合并:后出现的非空槽位覆盖前一个。
    pub fn merge(&mut self, patch: Self) {
        if patch.background.is_some() {
            self.background = patch.background;
        }
        if patch.highlight.is_some() {
            self.highlight = patch.highlight;
        }
        if patch.menu.is_some() {
            self.menu = patch.menu;
        }
    }
}

/// 截图结果;`file` 是宿主临时目录里的 BMP。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Shot {
    /// 截图文件路径。
    pub file: String,
    /// 像素宽度。
    pub width: i64,
    /// 像素高度。
    pub height: i64,
}

/// 识别结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OcrText {
    /// 识别出的文字,可能为空。
    pub text: String,
    /// 实际使用的语言标签;PP-OCR 报 `ppocr`。
    #[serde(default)]
    pub lang: String,
}

/// 上屏结果。
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Delivered {
    /// 是否真的送出了字符;空文本为 false。
    pub delivered: bool,
}

/// 宿主选择面板的结果。
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PanelResult {
    /// 用户选中的下标;取消或关闭为 `None`。
    #[serde(default)]
    pub selected: Option<u32>,
}

/// 剪贴板读取结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClipboardText {
    /// 纯文本内容;没有文本时为空串。
    #[serde(default)]
    pub text: String,
}

/// 扩展存储读取结果。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreValue {
    /// 键不存在时为 `None`。
    #[serde(default)]
    pub value: Option<String>,
}

/// 扩展存储键列表。
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreKeys {
    /// 全部键。
    #[serde(default)]
    pub keys: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_is_cleaned_before_serializing() {
        let state = SceneIn {
            page: 9,
            pages: 3,
            candidates: 2,
            selected: 7,
            accent: 0x1ff_ffff,
            time: f32::NAN,
            anim: f32::INFINITY,
            ..SceneIn::default()
        };
        let json = state.to_json().expect("序列化");
        assert!(json.contains("\"page\":3"));
        assert!(json.contains("\"selected\":1"));
        assert!(json.contains("\"accent\":16777215"));
        assert!(json.contains("\"time\":0.0"));
        assert!(json.contains("\"anim\":1.0"));
    }

    #[test]
    fn patch_rejects_bad_values() {
        assert!(ScenePatch::parse(r#"{"accent":16777216}"#).is_err());
        assert!(ScenePatch::parse(r#"{"future":true}"#).is_err());
        assert!(ScenePatch::parse(r#"{"effects":{"background":""}}"#).is_err());
        assert!(ScenePatch::parse(r#"{"accent":1193046}"#).is_ok());
    }
}
