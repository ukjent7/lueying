//! 截图 OCR 官方扩展:截图、识别、上屏;没有文字时发通知。
//!
//! 编成 `wasm32-unknown-unknown` 后放进包目录,清单见同目录 `manifest.json`。

use lueying_extguest::{Extension, capture_shot, notify_show, ocr_recognize, output_deliver};
use serde_json::json;

#[derive(Default)]
struct ShotOcr;

impl Extension for ShotOcr {
    fn command(&mut self, id: &str, _args: &str) -> Result<String, String> {
        if id != "run" {
            return Err(format!("没有命令 {id}"));
        }
        let shot = capture_shot()?;
        let ocr = ocr_recognize(&shot.file, None)?;
        let text = ocr.text.trim();
        let chars = text.chars().count();
        if chars == 0 {
            notify_show("截图 OCR", "没有识别到文字")?;
            return Ok(json!({
                "chars": 0,
                "lang": ocr.lang,
                "delivered": false,
            })
            .to_string());
        }
        let delivered = output_deliver(text)?;
        Ok(json!({
            "chars": chars,
            "lang": ocr.lang,
            "delivered": delivered.delivered,
        })
        .to_string())
    }
}

lueying_extguest::export!(ShotOcr);
