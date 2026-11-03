//! lueying-server 的共享部分:配置读写、设置页模型与扩展元数据等。

pub mod accents;

pub mod command_panel;

pub mod commit;

pub mod config;

mod config_file;

pub mod host_panel;

pub mod official_extensions;

pub mod settings_consent;

pub mod settings_ext;

pub mod settings_model;

pub mod settings_secret;

pub mod settings_schema;

pub mod settings_view;

mod settings_text;

pub mod seam;

pub mod skin;

/// 配置同步:载荷格式、摘要、冲突判定与 WebDAV 传输。
pub mod sync;

#[cfg(windows)]
pub mod ui_surface;

#[cfg(windows)]
pub mod services;

#[cfg(windows)]
pub mod settings_actions;

#[cfg(windows)]
pub mod settings_official;

#[cfg(windows)]
pub mod theme;
