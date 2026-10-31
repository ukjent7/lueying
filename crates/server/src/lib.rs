//! lueying-server 的共享部分:配置读写、设置页模型与扩展元数据等。

pub mod accents;

pub mod command_panel;

pub mod commit;

pub mod config;

mod config_file;

pub mod official_extensions;

pub mod settings_consent;

pub mod settings_ext;

pub mod settings_secret;

pub mod settings_schema;

mod settings_focus;

pub mod settings_view;

mod settings_text;

pub mod seam;

pub mod skin;

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
pub mod settings_official;

#[cfg(windows)]
pub mod settings_secret_edit;

#[cfg(windows)]
pub mod settings_win;

#[cfg(windows)]
pub mod theme;
