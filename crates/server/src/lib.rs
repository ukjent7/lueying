//! lueying-server 的共享部分:配置读写、主题与自动更新(常驻服务、设置窗与更新器共用)。

pub mod accents;

pub mod config;

pub mod settings_view;

#[cfg(windows)]
pub mod blit;

#[cfg(windows)]
pub mod font;

#[cfg(windows)]
pub mod services;

#[cfg(windows)]
pub mod settings_actions;

#[cfg(windows)]
pub mod settings_win;

#[cfg(windows)]
pub mod theme;

#[cfg(windows)]
pub mod update;
