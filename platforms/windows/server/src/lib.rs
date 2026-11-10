//! lueying-server 的共享部分:配置读写、设置页模型与扩展元数据等。

// 常驻服务与设置窗共用的分配器:降低碎片,服务端峰值工作集门禁受益;
// 纯分配器,导入表无网络符号,不碰「server 不链接网络库」的断言。
#[cfg(windows)]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod accents;

pub mod command_panel;

pub mod config;

mod config_file;

/// 词库导入:第三方词表转成用户词库的原生格式。
pub mod dict_import;

pub mod host_panel;

pub mod settings;

/// 配置备份包:备份导出与导入共用的文本格式。
pub mod bundle;

#[cfg(windows)]
pub mod ui_surface;

#[cfg(windows)]
pub mod services;

#[cfg(windows)]
pub mod theme;
