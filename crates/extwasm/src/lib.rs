//! 运行期 Wasm 扩展:用户写标准 Extism 插件,宿主用 Extism 运行期执行。
//!
//! 扩展是一个目录(`%APPDATA%\lueying\extensions\<id>\`),里面一份
//! `manifest.json` 与一个 `.wasm`。运行期不加载 DLL:Cranelift 编译、
//! 线性内存隔离、燃料计量、表与内存上限都在 Extism 运行期的沙箱里生效。
//!
//! 每个已加载扩展占一个 worker 线程,线程内持有插件与实例。命令在
//! worker 上串行执行,截图/识别/面板这类宿主服务是阻塞调用:宿主把调用
//! 交给服务缝的提供者,等它经 [`Done`] 回传,期间不碰引擎
//! 线程。外观派发同样在 worker 上跑,候选窗线程只拿合并后的补丁。
//!
//! # ABI
//!
//! 扩展就是标准 Extism 插件:wasm 模块从 `extism:host/env` 导入内核
//! 函数(分配、输入输出、错误),需要宿主服务时再从 `extism:host/user`
//! 导入 `lueying_<服务>`。命令按清单里的命令 id 调用同名导出,外观调用
//! `lueying_appearance` 导出。完整记录见 `docs/extensions/wasm.md`。
//!
//! # 边界
//!
//! - 线性内存上限 [`Limits::memory_bytes`],值栈上限 [`Limits::stack_bytes`]。
//! - 初始化与每次调用都有燃料预算;燃料烧完即 trap,连续 [`MAX_FAULTS`]
//!   次失败停用该包。
//! - 隐私字段([`SceneIn::private`])直接跳过,guest 不会收到那次状态。
//! - 高对比度下补丁里的效果被摘掉,只留主题色。
//! - 命令包空闲 [`IDLE_UNLOAD`] 后卸载;外观包常驻。

#![deny(missing_docs)]

pub mod bindings;
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
pub use host::{Finished, Limits};
pub use lueying_extmeta::consent::{ConsentBook, ConsentCheck, ConsentEntry};
pub use lueying_extmeta::metrics::{Budgets, ExtMetrics, ExtStat, ExtState, METRICS_VERSION};
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

/// 连续失败多少次停用扩展包;重载成功后计数清零。
pub const MAX_FAULTS: u32 = 3;

/// Wasm 模块文件的上限。官方 shot-ocr 编出来约几十 KB,16 MiB 足够大。
pub const MAX_WASM_BYTES: usize = 16 * 1024 * 1024;

/// 完整命令 id。
pub fn full_id(extension: &str, command: &str) -> String {
    format!("{extension}.{command}")
}
