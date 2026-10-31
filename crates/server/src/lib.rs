//! lueying-server 的共享部分:配置读写、主题与自动更新(常驻服务、设置窗与更新器共用)。

pub mod accents;

pub mod command_panel;

pub mod config;

mod config_file;

pub mod settings_consent;

pub mod settings_ext;

mod settings_focus;

pub mod settings_view;

mod settings_text;

pub mod update_manifest;

#[cfg(windows)]
pub mod blit;

#[cfg(windows)]
pub mod font;

#[cfg(windows)]
pub mod services;

#[cfg(windows)]
pub mod settings_a11y;

#[cfg(windows)]
pub mod settings_actions;

#[cfg(windows)]
pub mod settings_edit;

#[cfg(windows)]
pub mod settings_win;

#[cfg(windows)]
pub mod theme;

#[cfg(windows)]
pub mod update;
