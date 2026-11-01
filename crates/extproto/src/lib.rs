//! 扩展宿主与 Wasm guest 共用的协议类型。
//!
//! 这个 crate 同时编进宿主与 `wasm32-unknown-unknown`,所以只放数据契约,
//! 不放 wasmi、Windows 或任何宿主侧实现。场景协议仍然只描述面板状态:
//! 没有候选文字、没有宿主应用名、没有击键历史。

#![deny(missing_docs)]

use serde::{Deserialize, Serialize};

/// 场景协议版本。guest 在清单里不声明版本,宿主与 SDK 同步升级。
/// 2:补丁删掉 SkSL 效果槽位,只保留主题色。
pub const SCENE_API: u32 = 2;

/// 补丁 JSON 的总大小上限。
pub const MAX_PATCH_BYTES: usize = 8 * 1024;

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
}

impl ScenePatch {
    /// 解析并校验补丁。未知字段、非法主题色与超长补丁都拒绝。
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
        Ok(())
    }

    /// 按配置顺序合并后一个扩展的补丁:后出现的非空字段覆盖前一个。
    pub fn merge(&mut self, patch: Self) {
        if patch.accent.is_some() {
            self.accent = patch.accent;
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
        assert!(ScenePatch::parse(r#"{"effects":{}}"#).is_err());
        assert!(ScenePatch::parse(r#"{"accent":1193046}"#).is_ok());
    }
}
