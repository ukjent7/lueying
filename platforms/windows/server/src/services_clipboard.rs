//! 剪贴板:读写纯文本并维护历史,后台轮询供官方扩展展示。

use std::collections::VecDeque;

use std::sync::Mutex;

use std::sync::OnceLock;

use std::time::Duration;

use lueying_extwasm::{Done, Provider};

use lueying_extwasm::{ClipboardHistory, ClipboardHistoryItem};

use serde_json::json;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};

use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};

use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

/// 剪贴板:读写纯文本,并维护最近复制过的条目。
///
/// `read`/`write` 走 `CF_UNICODETEXT`;后台每 400ms 轮询系统剪贴板,
/// 把外部复制的文本记入历史,供「剪贴板历史」官方扩展展示。
pub struct ClipboardProvider;

/// `CF_UNICODETEXT` 的标准值;避免为单个常量引入整个剪贴板格式枚举。
const CF_UNICODETEXT: u32 = 13;
/// 历史最多保留多少条;200 条足够覆盖一天的高频复制。
const CLIPBOARD_HISTORY_MAX: usize = 200;

#[derive(Default)]
struct ClipboardHistoryStore {
    items: VecDeque<ClipboardHistoryItem>,
    /// 上一次看到的剪贴板文本;用来去重轮询里的重复读。
    last: String,
}

static CLIPBOARD_HISTORY: OnceLock<Mutex<ClipboardHistoryStore>> = OnceLock::new();
static CLIPBOARD_MONITOR: std::sync::Once = std::sync::Once::new();

impl ClipboardProvider {
    /// 建提供者并启动一次系统剪贴板轮询线程。
    pub fn new() -> Self {
        CLIPBOARD_MONITOR.call_once(|| {
            let spawned = std::thread::Builder::new()
                .name("clipboard-history".into())
                .spawn(|| {
                    loop {
                        std::thread::sleep(Duration::from_millis(400));
                        if let Ok(text) = read_clipboard() {
                            record_clipboard(&text);
                        }
                    }
                });
            if spawned.is_err() {
                println!("剪贴板历史轮询线程启动失败");
            }
        });
        Self
    }
}

impl Default for ClipboardProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Provider for ClipboardProvider {
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        match method {
            "read" => {
                let result = read_clipboard();
                if let Ok(text) = &result {
                    record_clipboard(text);
                }
                done.complete(result.map(|text| json!({ "text": text }).to_string()));
            }
            "write" => {
                let parsed: serde_json::Value =
                    serde_json::from_str(args).map_err(|e| e.to_string())?;
                let text = parsed["text"]
                    .as_str()
                    .ok_or_else(|| "缺 text".to_string())?;
                let result = write_clipboard(text);
                if result.is_ok() {
                    record_clipboard(text);
                }
                done.complete(result.map(|()| "{}".to_string()));
            }
            "history" => {
                let limit = serde_json::from_str::<serde_json::Value>(args)
                    .ok()
                    .and_then(|value| value["limit"].as_u64())
                    .unwrap_or(30)
                    .min(CLIPBOARD_HISTORY_MAX as u64) as usize;
                let store = clipboard_history_store()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let items: Vec<ClipboardHistoryItem> =
                    store.items.iter().take(limit).cloned().collect();
                drop(store);
                let payload = ClipboardHistory { items };
                done.complete(
                    serde_json::to_string(&payload)
                        .map_err(|error| format!("剪贴板历史序列化失败: {error}")),
                );
            }
            "remember" => {
                let parsed: serde_json::Value =
                    serde_json::from_str(args).map_err(|e| e.to_string())?;
                let text = parsed["text"]
                    .as_str()
                    .ok_or_else(|| "缺 text".to_string())?
                    .trim();
                if !text.is_empty() {
                    record_tagged_clipboard(text, parsed["tag"].as_str().unwrap_or(""));
                }
                done.complete(Ok("{}".to_string()));
            }
            "clear" => {
                let mut store = clipboard_history_store()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                store.items.clear();
                save_clipboard_history(&store);
                done.complete(Ok("{}".to_string()));
            }
            _ => return Err(format!("clipboard 没有方法 {method:?}")),
        }
        Ok(())
    }
}

/// 历史存储;首次访问时从磁盘读回上次的历史。
fn clipboard_history_store() -> &'static Mutex<ClipboardHistoryStore> {
    CLIPBOARD_HISTORY.get_or_init(|| {
        let items = crate::config::clipboard_history_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<ClipboardHistory>(&bytes).ok())
            .map(|history| history.items.into_iter().collect())
            .unwrap_or_default();
        Mutex::new(ClipboardHistoryStore {
            items,
            last: String::new(),
        })
    })
}

/// 记一条复制内容:空文本忽略,重复内容提到最前,总长封顶。
fn record_clipboard(text: &str) {
    record_tagged_clipboard(text, "");
}

/// 记一条内容并打来源标签;普通复制标签为空,OCR/语音管道带上来源。
pub(super) fn record_tagged_clipboard(text: &str, tag: &str) {
    if text.is_empty() {
        return;
    }
    let mut store = clipboard_history_store()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let previous_tag = store
        .items
        .iter()
        .find(|item| item.text == text)
        .map(|item| item.tag.clone());
    // 同一条内容连续出现时不重复记;带新标签的管道写入仍更新来源。
    if store.last == text && tag.is_empty() {
        return;
    }
    store.last = text.to_string();
    store.items.retain(|item| item.text != text);
    store.items.push_front(ClipboardHistoryItem {
        text: text.to_string(),
        time: now_ms(),
        tag: previous_tag.unwrap_or_default(),
    });
    if !tag.is_empty()
        && let Some(item) = store.items.front_mut()
    {
        item.tag = tag.to_string();
    }
    while store.items.len() > CLIPBOARD_HISTORY_MAX {
        store.items.pop_back();
    }
    save_clipboard_history(&store);
}

fn save_clipboard_history(store: &ClipboardHistoryStore) {
    let Some(path) = crate::config::clipboard_history_path() else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    let payload = ClipboardHistory {
        items: store.items.iter().cloned().collect(),
    };
    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

fn read_clipboard() -> Result<String, String> {
    unsafe {
        OpenClipboard(None).map_err(|e| format!("打开剪贴板失败: {e}"))?;
        let result = (|| {
            let Ok(handle) = GetClipboardData(CF_UNICODETEXT) else {
                return Ok(String::new());
            };
            let memory = HGLOBAL(handle.0);
            let pointer = GlobalLock(memory);
            if pointer.is_null() {
                return Err("锁定剪贴板内存失败".to_string());
            }
            let data = pointer as *const u16;
            let mut length = 0usize;
            while *data.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(data, length));
            let _ = GlobalUnlock(memory);
            Ok(text)
        })();
        let _ = CloseClipboard();
        result
    }
}

fn write_clipboard(text: &str) -> Result<(), String> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        OpenClipboard(None).map_err(|e| format!("打开剪贴板失败: {e}"))?;
        let result = (|| {
            EmptyClipboard().map_err(|e| format!("清空剪贴板失败: {e}"))?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2)
                .map_err(|e| format!("分配剪贴板内存失败: {e}"))?;
            let pointer = GlobalLock(memory);
            if pointer.is_null() {
                let _ = GlobalFree(Some(memory));
                return Err("锁定剪贴板内存失败".to_string());
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), pointer as *mut u16, units.len());
            let _ = GlobalUnlock(memory);
            match SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))) {
                Ok(_) => Ok(()),
                Err(e) => {
                    let _ = GlobalFree(Some(memory));
                    Err(format!("写入剪贴板失败: {e}"))
                }
            }
        })();
        let _ = CloseClipboard();
        result
    }
}
