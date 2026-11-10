//! 离线语音输入官方扩展:按热键录一小段话,经纯 CPU ASR 识别后上屏。
//!
//! 录音由宿主 `audio` 服务完成（16 kHz 单声道 WAV），识别交给原生
//! `sensevoice` 扩展；模型与可执行文件缺失时给用户明确提示。

use lueying_ext::guest::Extension;
use serde_json::json;

/// 一次录音的固定时长;短句足够,也避免误触长期占用麦克风。
const RECORD_SECONDS: u32 = 8;

#[derive(Default)]
struct VoiceAsr;

impl Extension for VoiceAsr {
    fn command(&mut self, id: &str, _args: &str) -> Result<String, String> {
        if id != "dictate" && id != "run" {
            return Err(format!("没有命令 {id}"));
        }
        notify_show("离线语音输入", "正在聆听……")?;
        let recording = audio_record(RECORD_SECONDS)?;
        let result = match asr_recognize(&recording.file) {
            Ok(text) => text,
            Err(error) => {
                notify_show("离线语音输入", &format!("识别失败：{error}"))?;
                return Err(error);
            }
        };
        let text = result.text.trim();
        if text.is_empty() {
            notify_show("离线语音输入", "没有识别到语音")?;
            return Ok(json!({ "chars": 0, "delivered": false }).to_string());
        }
        let delivered = output_deliver(text)?;
        Ok(json!({
            "chars": text.chars().count(),
            "delivered": delivered.delivered,
        })
        .to_string())
    }
}

lueying_ext::export_component!(VoiceAsr, commands: [dictate]);
