//! 运行期 Wasm 扩展:用户写标准 Wasm 组件(WASI 0.3 组件模型),宿主用
//! wasmtime 的组件绑定执行。
//!
//! 扩展是一个目录(`%APPDATA%\lueying\extensions\<id>\`),里面一份
//! `manifest.json` 与一个 `extension.wasm` 组件。运行期不加载 DLL:
//! Cranelift 编译与线性内存隔离由 wasmtime 提供。宿主不设资源预算,扩展
//! 跑多久、占多少内存由扩展自己决定。
//!
//! 每个已加载扩展占一个 worker 线程,线程内持有引擎、存储与类型化绑定。
//! 命令在 worker 上串行执行,截图/识别/面板这类宿主服务是阻塞调用:宿主
//! 把调用交给服务缝的提供者,等它经 [`Done`] 回传,期间不碰引擎线程。
//! 外观派发同样在 worker 上跑,候选窗线程只拿合并后的补丁。
//!
//! # ABI
//!
//! 组件按 `lueying.wit` 契约(`lueying:ext@2.0.0`)导出 `entry` 接口:
//! `init`/`command`/`appearance`;宿主服务是 WIT 导入接口,按包路径镜像。
//! 作者侧 SDK 见 `crates/extguest`。完整记录见 `docs/extensions/wasm.md`。
//!
//! # 边界
//!
//! - 隐私字段([`SceneIn::private`])直接跳过,guest 不会收到那次状态。
//! - 高对比度下补丁里的效果被摘掉,只留主题色。
//! - 命令包空闲 [`IDLE_UNLOAD`] 后卸载;外观包常驻。

#![deny(missing_docs)]

pub mod bindings;
pub(crate) mod component;
mod host;
mod manifest;
pub mod native;
mod pack;
mod runtime;
mod services;

#[cfg(test)]
mod test_guest;

use std::path::Path;
use std::sync::Arc;

pub use bindings::{Binding, BindingTable, Hotkey, Input, Modifier};
pub use host::Finished;
pub use lueying_extmeta::consent::{ConsentBook, ConsentCheck, ConsentEntry};
pub use lueying_extmeta::metrics::{ExtMetrics, ExtStat, ExtState, METRICS_VERSION};
pub use lueying_extmeta::{
    Activate, Command, EXT_API, MAX_MANIFEST_BYTES, MAX_TEXT_CHARS, Manifest, Permission, Service,
    Setting, SettingValue, When,
};
pub use lueying_extproto::{
    AsrText, AudioRecording, ClipboardHistory, ClipboardHistoryItem, ClipboardText, Delivered,
    MAX_PATCH_BYTES, MAX_RESULT_BYTES, OcrText, PanelResult, SCENE_API, SceneIn, ScenePatch, Shot,
    StoreKeys, StoreValue,
};
pub use pack::{Pack, Poll};
pub use runtime::{CommandInfo, IDLE_UNLOAD, Runtime, Scope, manifest_at};
pub use services::{AuditOutcome, AuditSink, Done, PrivacyCheck, Provider, Registry};

/// 热加载前的同意检查;返回 `Err` 时新内容不生效,旧版本继续跑。
pub type ConsentGate = Arc<dyn Fn(&Manifest, &Path) -> Result<(), String> + Send + Sync>;

/// Wasm 模块文件的上限。官方 shot-ocr 编出来约几十 KB,16 MiB 足够大。
pub const MAX_WASM_BYTES: usize = 16 * 1024 * 1024;

/// 声明 `engine` 的扩展(内嵌 JS 引擎、CPython 等)的模块上限;这类
/// 工具链的产物自带整个运行时,几十 MiB 起步。
pub const MAX_WASM_BYTES_ENGINE: usize = 64 * 1024 * 1024;

/// 完整命令 id。
pub fn full_id(extension: &str, command: &str) -> String {
    format!("{extension}.{command}")
}
