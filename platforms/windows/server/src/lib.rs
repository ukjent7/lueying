//! lueying-server 的共享部分:配置读写、主题令牌与扩展宿主连接。
//!
//! 设置窗、更新、备份与扩展商店都在 `lueying-settings`(独立进程,可联网);
//! 这里只放常驻 Server 与设置窗共用的数据与外观规则。

// 常驻服务与设置窗共用的分配器:降低碎片,服务端峰值工作集门禁受益;
// 纯分配器,导入表无网络符号,不碰「server 不链接网络库」的断言。
#[cfg(windows)]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod accents;

pub mod config;
pub mod ext_link;
pub mod typing_store;

mod config_file;

pub mod host_panel;

#[cfg(windows)]
pub mod ui_surface;

#[cfg(windows)]
pub mod services;

#[cfg(windows)]
pub mod theme;
