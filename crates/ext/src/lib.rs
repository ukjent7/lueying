//! 掠影扩展的清单、包与协议,一个 crate 按 feature 取用。内建模块与输入方案的
//! 契约在 `lueying-core`(编译期接入,不走本 crate)。
//!
//! - [`manifest`]:扩展包清单、权限、同意记录与运行指标,宿主、设置窗与 CLI 共用。
//! - [`proto`]:宿主与 Wasm guest 共用的场景协议与服务结果。
//! - [`proxy`]:出站代理的选择(环境变量与 Windows 系统代理),设置窗与扩展宿主共用。
//! - `guest`:Wasm 扩展作者 SDK,可编到 `wasm32-unknown-unknown`。
//! - `host`:Wasm 组件与原生进程的宿主运行时。

#![deny(missing_docs)]

pub mod manifest;
pub mod proto;
pub mod proxy;

#[cfg(feature = "guest")]
pub mod guest;
#[cfg(feature = "host")]
pub mod host;
