//! 候选窗状态事件:宿主推给订阅了 `candidate-state` 的原生扩展。
//!
//! 一条事件是候选窗此刻的完整快照,宿主只保留最新一条,扩展丢掉中间状态不影响结果。
//! 候选文字和组合拼音属于 `candidate-text` 权限;没有这项权限,或字段是私密字段时,
//! 扩展只能拿到位置与可见性。

use serde::{Deserialize, Serialize};

/// 宿主调用扩展时使用的方法名。
pub const METHOD: &str = "candidate.state";

/// 候选窗状态快照。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateState {
    /// 候选窗当前是否显示;淡出期间仍为 `true`。
    pub visible: bool,
    /// 窗口里是否有组合内容;淡出与停靠时为 `false`。
    pub composing: bool,
    /// 屏幕物理像素下的窗口矩形:x、y、宽、高。
    pub rect: [i32; 4],
    /// 私密字段:只给位置与可见性,文字一律留空。
    pub private: bool,
    /// 组合拼音;没有 `candidate-text` 权限时留空。
    pub raw: String,
    /// 当前页码(从 0 开始)。
    pub page: usize,
    /// 每页候选数。
    pub page_size: usize,
    /// 页内高亮下标。
    pub highlight: usize,
    /// 当前页候选;没有 `candidate-text` 权限时留空。
    pub candidates: Vec<String>,
}

impl CandidateState {
    /// 按订阅者的权限裁掉文字,编成发给扩展的 JSON。
    pub fn for_subscriber(&self, allow_text: bool) -> String {
        let mut state = self.clone();
        if !allow_text || self.private {
            state.raw.clear();
            state.candidates.clear();
        }
        serde_json::to_string(&state).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CandidateState {
        CandidateState {
            visible: true,
            composing: true,
            rect: [100, 200, 320, 48],
            private: false,
            raw: "nihao".to_string(),
            page: 0,
            page_size: 5,
            highlight: 0,
            candidates: vec!["你好".to_string(), "泥好".to_string()],
        }
    }

    #[test]
    fn subscriber_with_text_permission_gets_everything() {
        let json: CandidateState =
            serde_json::from_str(&sample().for_subscriber(true)).expect("合法 JSON");
        assert_eq!(json, sample());
    }

    #[test]
    fn subscriber_without_text_permission_gets_geometry_only() {
        let json: CandidateState =
            serde_json::from_str(&sample().for_subscriber(false)).expect("合法 JSON");
        assert!(json.visible && json.composing);
        assert_eq!(json.rect, [100, 200, 320, 48]);
        assert!(json.raw.is_empty());
        assert!(json.candidates.is_empty());
    }

    #[test]
    fn private_field_hides_text_even_with_permission() {
        let mut state = sample();
        state.private = true;
        let json: CandidateState =
            serde_json::from_str(&state.for_subscriber(true)).expect("合法 JSON");
        assert!(json.private);
        assert!(json.raw.is_empty());
        assert!(json.candidates.is_empty());
        assert_eq!(json.rect, [100, 200, 320, 48]);
    }
}
