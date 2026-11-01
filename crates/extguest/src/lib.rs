//! 掠影 Wasm 扩展的同步 Rust SDK。
//!
//! 扩展作者实现 [`Extension`],用 [`export!`] 导出入口;命令与服务都是同步
//! 调用,宿主在扩展自己的 worker 上阻塞等待。服务结果写进一块固定的结果
//! 缓冲区,超过 64 KiB 时宿主回一个错误信封。
//!
//! 这个 crate 同时编进 `wasm32-unknown-unknown` 与宿主测试目标:非 Wasm
//! 目标上的宿主导入是返回 `-1` 的桩,方便在普通 `cargo test` 里检查逻辑。

#![deny(missing_docs)]

use std::alloc::Layout;
use std::sync::{Mutex, MutexGuard};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

pub use lueying_extproto::{
    ClipboardText, Delivered, EffectsPatch, OcrText, PanelResult, SCENE_API, SceneIn, ScenePatch,
    Shot, StoreKeys, StoreValue,
};

/// 结果缓冲区的字节数。服务结果超过这个值时宿主回错误信封。
const RESULT_CAP: usize = 64 * 1024;

/// 宿主写服务结果、guest 读结果的固定缓冲区。
static RESULT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// 扩展入口。
///
/// 宿主只调用清单里声明过的命令;`appearance` 只在清单 `appearance: true`
/// 时被派发。实现类型必须能放进静态 `Mutex`,也就是 `Send`。
pub trait Extension: Send {
    /// 初始化;返回 `Err` 会让扩展加载失败。
    fn init(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// 执行一条命令;参数与返回值都是 JSON 文本。
    fn command(&mut self, id: &str, _args: &str) -> Result<String, String> {
        Err(format!("扩展没有命令 {id}"))
    }

    /// 返回一帧外观补丁;`Ok(None)` 表示这一帧不改。
    fn appearance(&mut self, _scene: &SceneIn) -> Result<Option<ScenePatch>, String> {
        Ok(None)
    }
}

/// 宿主导入的服务函数签名:方法名与参数都从 guest 内存读。
type ServiceFn = unsafe extern "C" fn(i32, i32, i32, i32) -> i32;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "lueying")]
unsafe extern "C" {
    #[link_name = "emit"]
    fn host_emit(ptr: i32, len: i32, ok: i32);
    #[link_name = "timeout"]
    fn host_timeout(ms: i64) -> i32;
    #[link_name = "capture"]
    fn host_capture(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i32;
    #[link_name = "ocr"]
    fn host_ocr(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i32;
    #[link_name = "output"]
    fn host_output(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i32;
    #[link_name = "panel"]
    fn host_panel(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i32;
    #[link_name = "clipboard"]
    fn host_clipboard(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i32;
    #[link_name = "store"]
    fn host_store(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i32;
    #[link_name = "notify"]
    fn host_notify(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i32;
    #[link_name = "hotkey"]
    fn host_hotkey(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i32;
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_emit(_ptr: i32, _len: i32, _ok: i32) {}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_timeout(_ms: i64) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_capture(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_ocr(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_output(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_panel(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_clipboard(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_store(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_notify(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_hotkey(_a: i32, _b: i32, _c: i32, _d: i32) -> i32 {
    -1
}

/// 截取整块虚拟桌面,返回临时 BMP 的路径与尺寸。
pub fn capture_shot() -> Result<Shot, String> {
    call_typed(host_capture, "shot", "")
}

/// 识别一张位图;`lang` 只对系统 OCR 有意义,`None` 用系统默认语言。
pub fn ocr_recognize(file: &str, lang: Option<&str>) -> Result<OcrText, String> {
    let args = json!({ "file": file, "lang": lang }).to_string();
    call_typed(host_ocr, "recognize", &args)
}

/// 把文本送进当前焦点;空文本返回 `delivered: false`。
pub fn output_deliver(text: &str) -> Result<Delivered, String> {
    let args = json!({ "text": text }).to_string();
    call_typed(host_output, "deliver", &args)
}

/// 显示宿主选择面板,等用户选中或关闭。
pub fn panel_show(title: &str, items: &[String]) -> Result<PanelResult, String> {
    let args = json!({ "title": title, "items": items }).to_string();
    call_typed(host_panel, "show", &args)
}

/// 读剪贴板纯文本。
pub fn clipboard_read() -> Result<ClipboardText, String> {
    call_typed(host_clipboard, "read", "")
}

/// 写剪贴板纯文本。
pub fn clipboard_write(text: &str) -> Result<(), String> {
    let args = json!({ "text": text }).to_string();
    call_unit(host_clipboard, "write", &args)
}

/// 读扩展私有存储的一个键。
pub fn store_get(key: &str) -> Result<StoreValue, String> {
    let args = json!({ "key": key }).to_string();
    call_typed(host_store, "get", &args)
}

/// 写扩展私有存储的一个键。
pub fn store_set(key: &str, value: &str) -> Result<(), String> {
    let args = json!({ "key": key, "value": value }).to_string();
    call_unit(host_store, "set", &args)
}

/// 删除扩展私有存储的一个键。
pub fn store_remove(key: &str) -> Result<(), String> {
    let args = json!({ "key": key }).to_string();
    call_unit(host_store, "remove", &args)
}

/// 列出扩展私有存储的全部键。
pub fn store_list() -> Result<StoreKeys, String> {
    call_typed(host_store, "list", "")
}

/// 发一条系统通知;v1 落在宿主日志里。
pub fn notify_show(title: &str, body: &str) -> Result<(), String> {
    let args = json!({ "title": title, "body": body }).to_string();
    call_unit(host_notify, "show", &args)
}

/// 注册一个全局热键;组合由 `modifiers` 与 `key` 分开给。
pub fn hotkey_register(id: &str, modifiers: &[&str], key: &str) -> Result<(), String> {
    let args = json!({ "id": id, "modifiers": modifiers, "key": key }).to_string();
    call_unit(host_hotkey, "register", &args)
}

/// 注销一个全局热键。
pub fn hotkey_unregister(id: &str) -> Result<(), String> {
    let args = json!({ "id": id }).to_string();
    call_unit(host_hotkey, "unregister", &args)
}

/// 等一次热键触发;隐私字段下返回取消错误。
pub fn hotkey_press(id: &str) -> Result<(), String> {
    let args = json!({ "id": id }).to_string();
    call_unit(host_hotkey, "press", &args)
}

/// 可取消的等待;宿主卸载扩展时返回错误。
pub fn timeout_sleep(ms: u64) -> Result<(), String> {
    let code = unsafe { host_timeout(ms.min(600_000) as i64) };
    match code {
        0 => Ok(()),
        -1 => Err("等待被取消".into()),
        other => Err(format!("等待失败({other})")),
    }
}

/// 导出扩展入口。
///
/// 用法:
///
/// ```ignore
/// #[derive(Default)]
/// struct MyExtension;
///
/// impl lueying_extguest::Extension for MyExtension {
///     fn command(&mut self, id: &str, _args: &str) -> Result<String, String> {
///         Ok("{}".into())
///     }
/// }
///
/// lueying_extguest::export!(MyExtension);
/// ```
#[macro_export]
macro_rules! export {
    ($ty:ty) => {
        const _: () = {
            fn with<R>(f: impl FnOnce(&mut $ty) -> R) -> R {
                static EXTENSION: ::std::sync::Mutex<::std::option::Option<$ty>> =
                    ::std::sync::Mutex::new(::std::option::Option::None);
                let mut guard = EXTENSION.lock().unwrap_or_else(|error| error.into_inner());
                let slot = guard.get_or_insert_with(<$ty as ::std::default::Default>::default);
                f(slot)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_init() -> i32 {
                match with(|ext| $crate::Extension::init(ext)) {
                    Ok(()) => 0,
                    Err(error) => {
                        $crate::__private::emit_error(&error);
                        1
                    }
                }
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_command(
                id_ptr: i32,
                id_len: i32,
                args_ptr: i32,
                args_len: i32,
            ) -> i32 {
                let id = match unsafe { $crate::__private::read_input(id_ptr, id_len) } {
                    Ok(text) => text,
                    Err(error) => {
                        $crate::__private::emit_error(&error);
                        return 1;
                    }
                };
                let args = match unsafe { $crate::__private::read_input(args_ptr, args_len) } {
                    Ok(text) => text,
                    Err(error) => {
                        $crate::__private::emit_error(&error);
                        return 1;
                    }
                };
                match with(|ext| $crate::Extension::command(ext, &id, &args)) {
                    Ok(text) => {
                        $crate::__private::emit_ok(&text);
                        0
                    }
                    Err(error) => {
                        $crate::__private::emit_error(&error);
                        1
                    }
                }
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_appearance(scene_ptr: i32, scene_len: i32) -> i32 {
                let text = match unsafe { $crate::__private::read_input(scene_ptr, scene_len) } {
                    Ok(text) => text,
                    Err(error) => {
                        $crate::__private::emit_error(&error);
                        return 1;
                    }
                };
                let scene = match $crate::__private::parse_scene(&text) {
                    Ok(scene) => scene,
                    Err(error) => {
                        $crate::__private::emit_error(&error);
                        return 1;
                    }
                };
                match with(|ext| $crate::Extension::appearance(ext, &scene)) {
                    Ok(Some(patch)) => match $crate::__private::patch_json(&patch) {
                        Ok(text) => {
                            $crate::__private::emit_ok(&text);
                            0
                        }
                        Err(error) => {
                            $crate::__private::emit_error(&error);
                            1
                        }
                    },
                    Ok(None) => {
                        $crate::__private::emit_ok("");
                        0
                    }
                    Err(error) => {
                        $crate::__private::emit_error(&error);
                        1
                    }
                }
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_alloc(len: i32) -> i32 {
                $crate::__private::alloc(len)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_free(ptr: i32, len: i32) {
                $crate::__private::free(ptr, len)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_result_ptr() -> i32 {
                $crate::__private::result_ptr()
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_result_cap() -> i32 {
                $crate::__private::result_cap()
            }
        };
    };
}

#[derive(Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    value: serde_json::Value,
    #[serde(default)]
    error: String,
}

fn result_buffer() -> MutexGuard<'static, Vec<u8>> {
    let mut guard = RESULT.lock().unwrap_or_else(|error| error.into_inner());
    if guard.len() != RESULT_CAP {
        guard.resize(RESULT_CAP, 0);
    }
    guard
}

fn emit_raw(bytes: &[u8], ok: bool) {
    unsafe {
        host_emit(
            bytes.as_ptr() as i32,
            bytes.len() as i32,
            if ok { 1 } else { 0 },
        );
    }
}

fn call(raw: ServiceFn, method: &str, args: &str) -> Result<serde_json::Value, String> {
    let code = unsafe {
        raw(
            method.as_ptr() as i32,
            method.len() as i32,
            args.as_ptr() as i32,
            args.len() as i32,
        )
    };
    if code < 0 {
        return Err("宿主没有回传服务结果".into());
    }
    let bytes = {
        let buffer = result_buffer();
        let end = (code as usize).min(buffer.len());
        buffer[..end].to_vec()
    };
    if bytes.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    let envelope: Envelope =
        serde_json::from_slice(&bytes).map_err(|error| format!("服务结果解析失败: {error}"))?;
    if envelope.ok {
        Ok(envelope.value)
    } else {
        Err(envelope.error)
    }
}

fn call_typed<T: DeserializeOwned>(raw: ServiceFn, method: &str, args: &str) -> Result<T, String> {
    let value = call(raw, method, args)?;
    serde_json::from_value(value).map_err(|error| format!("服务结果解析失败: {error}"))
}

fn call_unit(raw: ServiceFn, method: &str, args: &str) -> Result<(), String> {
    call(raw, method, args).map(|_| ())
}

/// `export!` 展开时用到的内部符号;不属于公开 API。
#[doc(hidden)]
pub mod __private {
    pub use crate::{
        alloc, emit_error, emit_ok, free, parse_scene, patch_json, read_input, result_cap,
        result_ptr,
    };
}

/// 给宿主写输入分配一块 guest 内存。
#[doc(hidden)]
pub fn alloc(len: i32) -> i32 {
    if len <= 0 {
        return 0;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return 0;
    };
    let pointer = unsafe { std::alloc::alloc(layout) };
    if pointer.is_null() { 0 } else { pointer as i32 }
}

/// 释放宿主写输入用的 guest 内存。
#[doc(hidden)]
pub fn free(ptr: i32, len: i32) {
    if ptr <= 0 || len <= 0 {
        return;
    }
    let Ok(layout) = Layout::from_size_align(len as usize, 1) else {
        return;
    };
    unsafe { std::alloc::dealloc(ptr as *mut u8, layout) };
}

/// 结果缓冲区的起始地址;宿主实例化后读一次并缓存。
#[doc(hidden)]
pub fn result_ptr() -> i32 {
    result_buffer().as_ptr() as i32
}

/// 结果缓冲区的字节数。
#[doc(hidden)]
pub fn result_cap() -> i32 {
    RESULT_CAP as i32
}

/// 从宿主写进 guest 内存的输入读 UTF-8 文本。
#[doc(hidden)]
pub unsafe fn read_input(ptr: i32, len: i32) -> Result<String, String> {
    if len < 0 {
        return Err("输入长度为负".into());
    }
    if len == 0 {
        return Ok(String::new());
    }
    if ptr <= 0 {
        return Err("输入指针为空".into());
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    String::from_utf8(bytes.to_vec()).map_err(|_| "输入不是 UTF-8".into())
}

/// 解析宿主传来的场景 JSON。
#[doc(hidden)]
pub fn parse_scene(text: &str) -> Result<SceneIn, String> {
    SceneIn::parse(text)
}

/// 把补丁序列化成 JSON。
#[doc(hidden)]
pub fn patch_json(patch: &ScenePatch) -> Result<String, String> {
    serde_json::to_string(patch).map_err(|error| format!("补丁序列化失败: {error}"))
}

/// 把成功结果交给宿主。
#[doc(hidden)]
pub fn emit_ok(text: &str) {
    emit_raw(text.as_bytes(), true);
}

/// 把错误文本交给宿主。
#[doc(hidden)]
pub fn emit_error(text: &str) {
    emit_raw(text.as_bytes(), false);
}
