//! lueying-server 的共享部分:配置读写、主题与自动更新(常驻服务、设置窗与更新器共用)。

pub mod config;

#[cfg(windows)]
pub mod theme;

#[cfg(windows)]
pub mod update;
