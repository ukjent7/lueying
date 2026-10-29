//! 外观宿主的清单兼容层:类型与校验在 `lueying-extmeta`,这里只保留源码上限。
//!
//! 旧版外观包(`{ "api": 1, "main": "index.js" }`)与新清单都由
//! `lueying_extmeta::Manifest` 解析;外观宿主只要求包里带 `appearance`
//! 组件(或旧版顶层 `api`/`main`)。

pub use lueying_extmeta::{MAX_MANIFEST_BYTES, Manifest};

/// 入口脚本的大小上限。打包后的单文件 IIFE 通常几十 KB,留足余量。
pub const MAX_SOURCE_BYTES: usize = 512 * 1024;
