//! 脚本宿主的清单兼容层:类型与校验在 `lueying-extmeta`,这里只保留源码上限。
//!
//! 旧版外观包(`{ "api": 1, "main": "index.js" }`)、组件式外观包与 API 2
//! 脚本包都由 `lueying_extmeta::Manifest` 解析;脚本宿主只要求包里有脚本
//! 入口(`script_main`)。

pub use lueying_extmeta::{
    Activate, Command, MAX_MANIFEST_BYTES, Manifest, Permission, Service, When,
};

/// 入口脚本的大小上限。打包后的单文件 IIFE 通常几十 KB,留足余量。
pub const MAX_SOURCE_BYTES: usize = 512 * 1024;
