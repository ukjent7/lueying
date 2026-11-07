//! 剪贴板历史官方扩展:热键唤出最近复制条目,选中即粘贴。
//!
//! 历史由宿主 `clipboard` 服务在后台维护;这里只负责取最近 60 条,
//! 用宿主面板展示,用户选中后写回剪贴板并通过 `output` 注入当前焦点。

use lueying_extguest::{ClipboardHistoryItem, Extension};
use serde_json::json;

/// 面板条数上限;宿主面板最多 64 行,留几行给标题与滚动。
const PANEL_LIMIT: usize = 60;
/// 单行预览字符数;宿主面板单条上限 200,截断后仍有足够上下文。
const PREVIEW_CHARS: usize = 80;

#[derive(Default)]
struct ClipboardHistoryExt;

impl Extension for ClipboardHistoryExt {
    fn command(&mut self, id: &str, _args: &str) -> Result<String, String> {
        if id != "show" && id != "run" {
            return Err(format!("没有命令 {id}"));
        }
        let history = clipboard_history(PANEL_LIMIT)?;
        if history.items.is_empty() {
            notify_show("剪贴板历史", "还没有复制过文本")?;
            return Ok(json!({ "selected": null }).to_string());
        }
        let items: Vec<String> = history.items.iter().map(preview).collect();
        let result = panel_show("剪贴板历史", &items)?;
        let Some(index) = result.selected else {
            return Ok(json!({ "selected": null }).to_string());
        };
        let Some(item) = history.items.get(index as usize) else {
            return Err("面板返回的下标越界".to_string());
        };
        clipboard_write(&item.text)?;
        let delivered = output_deliver(&item.text)?;
        Ok(json!({
            "selected": index,
            "delivered": delivered.delivered,
        })
        .to_string())
    }
}

/// 面板预览:来源标签前置,换行与制表压成空格,超长截断。
fn preview(item: &ClipboardHistoryItem) -> String {
    let single = item.text.replace(['\r', '\n', '\t'], " ");
    let mut text: String = single.chars().take(PREVIEW_CHARS).collect();
    if single.chars().count() > PREVIEW_CHARS {
        text.push('…');
    }
    if item.tag.is_empty() {
        text
    } else {
        format!("[{}] {text}", item.tag)
    }
}

lueying_extguest::export!(ClipboardHistoryExt, commands: [show]);
