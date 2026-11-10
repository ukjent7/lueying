//! 掠影扩展的契约、清单与包,一个 crate 按 feature 取用:
//!
//! - 默认的 `contract`:内建模块契约 [`Module`] 与输入方案 [`Scheme`],
//!   核心功能与官方方案编译期接入。
//! - [`manifest`]:扩展包清单、权限、同意记录与运行指标,宿主、设置窗与 CLI 共用。
//! - [`proto`]:宿主与 Wasm guest 共用的场景协议与服务结果。
//! - `guest`:Wasm 扩展作者 SDK,可编到 `wasm32-unknown-unknown`。
//! - `host`:Wasm 组件与原生进程的宿主运行时。

#![deny(missing_docs)]

#[cfg(feature = "contract")]
mod contract;
#[cfg(feature = "contract")]
pub use contract::*;

pub mod manifest;
pub mod proto;

#[cfg(feature = "guest")]
pub mod guest;
#[cfg(feature = "host")]
pub mod host;
