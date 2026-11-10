//! 服务端与扩展宿主之间的行协议:stdio 上每行一条 JSON。
//!
//! 服务端启动 `lueying-ext-host` 并接管它的标准输入输出。两个方向都是
//! 「最新值」语义的槽(外观、上下文、候选快照)加一条有序队列(配置、
//! 命令、应答),写端在独立线程上跑,调用方从不等待宿主。

use serde::{Deserialize, Serialize};

use crate::proto::candidate::CandidateState;
use crate::proto::{SceneIn, ScenePatch};

/// 输入上下文:隐私门禁与热键 `when` 判断用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    /// 密码或隐私字段;扩展服务与热键一律停用。
    pub private: bool,
    /// 当前可编辑。
    pub editable: bool,
    /// 当前正在组合。
    pub composing: bool,
}

/// 服务端发给宿主的消息。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToHost {
    /// 生效配置:扩展顺序、绑定行与 OCR 后端。
    Config {
        /// 启用的扩展 id,按组合顺序。
        extensions: Vec<String>,
        /// 全局热键绑定行:(组合串, 完整命令 id)。
        bindings: Vec<(String, String)>,
        /// OCR 后端是否为 PP-OCR(否则用系统引擎)。
        ppocr: bool,
    },
    /// 最新外观状态;宿主忙时只保留最后一帧。
    Scene {
        /// 外观序号,补丁按它对号。
        seq: u64,
        /// 场景状态。
        scene: SceneIn,
        /// 收件人;只有私密场景带它,由服务端按联网与私密许可算出。`None` 表示全部外观扩展。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Vec<String>>,
    },
    /// 输入上下文变化。
    Context(Context),
    /// 候选窗状态快照,转给订阅了候选窗事件的扩展。
    Candidates {
        /// 候选窗此刻的完整快照。
        state: CandidateState,
    },
    /// 外部触发的命令(动作候选、命令执行)。
    Invoke {
        /// 完整命令 id。
        command: String,
        /// 来源,只用于日志。
        source: String,
    },
    /// 宿主请求的服务结果;`id` 对应 [`ToServer::Request`]。
    Reply {
        /// 请求编号。
        id: u64,
        /// 服务结果或错误文本。
        result: Result<String, String>,
    },
}

/// 宿主发给服务端的消息。
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToServer {
    /// 外观补丁,对应 [`ToHost::Scene`] 的序号;`None` 表示清空效果。
    Patch {
        /// 对应的外观序号。
        seq: u64,
        /// 补丁内容。
        patch: Option<ScenePatch>,
    },
    /// 已加载命令的标题表:动作候选与外部启动器用。
    Titles {
        /// `(完整命令 id, 标题)`。
        commands: Vec<(String, String)>,
    },
    /// 需要服务端完成的服务调用:输出、选择面板、上下文。
    Request {
        /// 请求编号,应答按它回填。
        id: u64,
        /// 发起调用的扩展 id(权限已在宿主侧核对)。
        caller: String,
        /// 服务名。
        service: String,
        /// 方法名。
        method: String,
        /// 参数 JSON。
        args: String,
    },
}

/// 一行 JSON;换行与结束符由传输层补。
pub fn encode<T: Serialize>(message: &T) -> Result<String, String> {
    serde_json::to_string(message).map_err(|error| format!("协议编码失败: {error}"))
}

/// 解一行 JSON;坏行由调用方丢弃,不拖垮连接。
pub fn decode<T: serde::de::DeserializeOwned>(line: &str) -> Result<T, String> {
    serde_json::from_str(line.trim()).map_err(|error| format!("协议解码失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_round_trips_with_tag() {
        let message = ToHost::Scene {
            seq: 7,
            scene: SceneIn::default(),
            to: None,
        };
        let line = encode(&message).unwrap();
        assert!(line.starts_with(r#"{"type":"scene","seq":7"#), "{line}");
        assert_eq!(decode::<ToHost>(&line).unwrap(), message);
    }

    #[test]
    fn reply_keeps_result_shape() {
        let message = ToHost::Reply {
            id: 3,
            result: Err("没有提供者".into()),
        };
        let line = encode(&message).unwrap();
        assert_eq!(decode::<ToHost>(&line).unwrap(), message);
    }

    #[test]
    fn garbage_line_is_an_error_not_a_panic() {
        assert!(decode::<ToServer>("not json").is_err());
        assert!(decode::<ToServer>(r#"{"type":"unknown"}"#).is_err());
    }
}
