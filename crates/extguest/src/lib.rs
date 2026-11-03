//! 掠影 Wasm 扩展的 Rust SDK(Extism ABI)。
//!
//! 扩展作者实现 [`Extension`],用 [`export!`] 声明命令;宿主按命令 id 调用
//! 同名导出,输入是参数 JSON,输出是结果 JSON。命令与服务都是同步调用,
//! 宿主在扩展自己的 worker 上阻塞等待。
//!
//! 产物是标准 Extism 插件:别的 Extism 宿主也跑得动,别的 Extism 插件放进
//! 掠影的扩展目录同样能被调用。宿主服务从 `extism:host/user` 导入,
//! 清单 `needs` 里没声明的服务在链接时就通不过。
//!
//! 这个 crate 同时编进 `wasm32-unknown-unknown` 与宿主测试目标:非 Wasm
//! 目标上的宿主导入是返回 `-1` 的桩,方便在普通 `cargo test` 里检查逻辑。

#![deny(missing_docs)]

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

pub use lueying_extproto::{
    ClipboardText, Delivered, FETCH_DEFAULT_LIMIT, FETCH_DEFAULT_TIMEOUT_MS, FetchArgs,
    FetchResult, OcrText, PanelResult, SCENE_API, SceneIn, ScenePatch, SelectedText, Shot,
    StoreKeys, StoreValue,
};

/// 扩展入口。
///
/// 宿主只调用清单里声明过的命令;`appearance` 只在清单 `appearance: true`
/// 时被派发。实现类型必须能放进静态 `Mutex`,也就是 `Send`。
pub trait Extension: Send + Default {
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

/// 宿主服务函数的签名:收一段 extism 内存里的 JSON,回一段 JSON 的偏移。
type ServiceFn = unsafe extern "C" fn(u64) -> u64;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "extism:host/env")]
unsafe extern "C" {
    fn input_length() -> u64;
    fn input_load_u8(offs: u64) -> u8;
    fn length(offs: u64) -> u64;
    fn alloc(len: u64) -> u64;
    fn free(offs: u64);
    fn output_set(offs: u64, len: u64);
    fn error_set(offs: u64);
    fn load_u8(offs: u64) -> u8;
    fn store_u8(offs: u64, value: u8);
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "extism:host/user")]
unsafe extern "C" {
    #[link_name = "lueying_timeout"]
    fn host_timeout(ms: i64) -> i32;
    #[link_name = "lueying_capture"]
    fn host_capture(request: u64) -> u64;
    #[link_name = "lueying_ocr"]
    fn host_ocr(request: u64) -> u64;
    #[link_name = "lueying_output"]
    fn host_output(request: u64) -> u64;
    #[link_name = "lueying_panel"]
    fn host_panel(request: u64) -> u64;
    #[link_name = "lueying_clipboard"]
    fn host_clipboard(request: u64) -> u64;
    #[link_name = "lueying_store"]
    fn host_store(request: u64) -> u64;
    #[link_name = "lueying_notify"]
    fn host_notify(request: u64) -> u64;
    #[link_name = "lueying_hotkey"]
    fn host_hotkey(request: u64) -> u64;
    #[link_name = "lueying_network"]
    fn host_network(request: u64) -> u64;
    #[link_name = "lueying_context"]
    fn host_context(request: u64) -> u64;
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn alloc(_len: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn free(_offset: u64) {}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn length(_offset: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn load_u8(_offset: u64) -> u8 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn store_u8(_offset: u64, _value: u8) {}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn input_length() -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn input_load_u8(_index: u64) -> u8 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn output_set(_offset: u64, _len: u64) {}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn error_set(_offset: u64) {}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_timeout(_ms: i64) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_capture(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_ocr(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_output(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_panel(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_clipboard(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_store(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_notify(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_hotkey(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_network(_request: u64) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
unsafe extern "C" fn host_context(_request: u64) -> u64 {
    0
}

/// 截取整块虚拟桌面,返回临时 BMP 的路径与尺寸。
pub fn capture_shot() -> Result<Shot, String> {
    call_typed(host_capture, "shot", "{}")
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
    call_typed(host_clipboard, "read", "{}")
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
    call_typed(host_store, "list", "{}")
}

/// 经独立 broker 发一次 HTTP(S) 请求;域名必须在清单 network.allow
/// 白名单里,宿主逐请求强制。响应体是文本;体积与时限缺省值见
/// [`FETCH_DEFAULT_LIMIT`] 与 [`FETCH_DEFAULT_TIMEOUT_MS`]。
pub fn network_fetch(options: &FetchArgs) -> Result<FetchResult, String> {
    let args =
        serde_json::to_string(options).map_err(|error| format!("联网参数序列化失败: {error}"))?;
    call_typed(host_network, "fetch", &args)
}

/// 读当前窗口里用户选中的文字;扩展清单要在 `capabilities` 里申请
/// `selected-text`,没申请就调用失败。没有选中文字时返回空串。
pub fn context_selection() -> Result<SelectedText, String> {
    call_typed(host_context, "selection", "{}")
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
/// 命令 id 就是导出的符号名,清单里的命令 id 要与它一致。用法:
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
/// lueying_extguest::export!(MyExtension, commands: [run]);
/// ```
#[macro_export]
macro_rules! export {
    ($ty:ty, commands: [$($command:ident),* $(,)?]) => {
        const _: () = {
            static EXTENSION: ::std::sync::Mutex<::std::option::Option<$ty>> =
                ::std::sync::Mutex::new(::std::option::Option::None);

            $(
                #[unsafe(no_mangle)]
                pub extern "C" fn $command() -> i32 {
                    let mut guard =
                        EXTENSION.lock().unwrap_or_else(|error| error.into_inner());
                    let slot = guard.get_or_insert_with(<$ty as ::std::default::Default>::default);
                    $crate::__private::run_command(slot, stringify!($command))
                }
            )*

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_appearance() -> i32 {
                let mut guard = EXTENSION.lock().unwrap_or_else(|error| error.into_inner());
                let slot = guard.get_or_insert_with(<$ty as ::std::default::Default>::default);
                $crate::__private::run_appearance(slot)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn lueying_init() -> i32 {
                let mut guard = EXTENSION.lock().unwrap_or_else(|error| error.into_inner());
                let slot = guard.get_or_insert_with(<$ty as ::std::default::Default>::default);
                $crate::__private::run_init(slot)
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

/// 调一次宿主服务;请求与响应都是 JSON。
fn call(raw: ServiceFn, method: &str, args: &str) -> Result<serde_json::Value, String> {
    let args = if args.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(args).unwrap_or_else(|_| serde_json::Value::String(args.to_string()))
    };
    let request = serde_json::json!({ "method": method, "args": args }).to_string();
    let offset = write_block(request.as_bytes());
    let response = unsafe { raw(offset) };
    if response == 0 {
        return Err("宿主没有回传服务结果".into());
    }
    let bytes = read_block(response);
    unsafe { free(response) };
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

/// 往 extism 内存写一块数据,返回块首偏移。
fn write_block(bytes: &[u8]) -> u64 {
    let offset = unsafe { alloc(bytes.len() as u64) };
    for (index, byte) in bytes.iter().enumerate() {
        unsafe { store_u8(offset + index as u64, *byte) };
    }
    offset
}

/// 从 extism 内存读一块数据。
fn read_block(offset: u64) -> Vec<u8> {
    let len = unsafe { length(offset) } as usize;
    let mut out = vec![0u8; len];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = unsafe { load_u8(offset + index as u64) };
    }
    out
}

/// 宿主传进来的输入;命令是参数 JSON,外观是场景 JSON。
fn input_string() -> Result<String, String> {
    let len = unsafe { input_length() } as usize;
    let mut bytes = vec![0u8; len];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = unsafe { input_load_u8(index as u64) };
    }
    String::from_utf8(bytes).map_err(|_| "宿主输入不是 UTF-8".to_string())
}

/// 把结果交给宿主。
fn set_output(bytes: &[u8]) {
    let offset = write_block(bytes);
    unsafe { output_set(offset, bytes.len() as u64) };
}

/// 把错误文本交给宿主。
fn set_error(text: &str) {
    let offset = write_block(text.as_bytes());
    unsafe { error_set(offset) };
}

/// `export!` 展开时用到的内部符号;不属于公开 API。
#[doc(hidden)]
pub mod __private {
    use super::*;

    /// 跑一条命令;返回 0 是成功。
    pub fn run_command<E: Extension>(ext: &mut E, id: &str) -> i32 {
        let args = match input_string() {
            Ok(text) => text,
            Err(error) => {
                set_error(&error);
                return -1;
            }
        };
        match ext.command(id, &args) {
            Ok(text) => {
                set_output(text.as_bytes());
                0
            }
            Err(error) => {
                set_error(&error);
                -1
            }
        }
    }

    /// 跑一帧外观;返回 0 是成功,输出是补丁 JSON。
    pub fn run_appearance<E: Extension>(ext: &mut E) -> i32 {
        let text = match input_string() {
            Ok(text) => text,
            Err(error) => {
                set_error(&error);
                return -1;
            }
        };
        let scene = match SceneIn::parse(&text) {
            Ok(scene) => scene,
            Err(error) => {
                set_error(&error);
                return -1;
            }
        };
        match ext.appearance(&scene) {
            Ok(Some(patch)) => match serde_json::to_string(&patch) {
                Ok(text) => {
                    set_output(text.as_bytes());
                    0
                }
                Err(error) => {
                    set_error(&format!("补丁序列化失败: {error}"));
                    -1
                }
            },
            Ok(None) => {
                set_output(b"");
                0
            }
            Err(error) => {
                set_error(&error);
                -1
            }
        }
    }

    /// 跑初始化;返回 0 是成功。
    pub fn run_init<E: Extension>(ext: &mut E) -> i32 {
        match ext.init() {
            Ok(()) => 0,
            Err(error) => {
                set_error(&error);
                -1
            }
        }
    }
}
