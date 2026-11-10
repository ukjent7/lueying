//! lueying-settings 的库部分:设置页模型、动作、备份、词库导入的文本格式,以及扩展安装(`ext install` / `ext add`)。
//!
//! 页面模型不碰 Windows,Linux 上直接跑单测;Slint 窗口与平台动作只在 Windows 编译。
//! 进程入口在 `main.rs`。设置窗是独立进程,更新与扩展安装都在这里联网;
//! 常驻 Server 不依赖本 crate。

pub mod bundle;
pub mod dict_import;
pub mod ext_add;
pub mod ext_cli;
pub mod ext_install;
pub mod ext_secret;
pub mod net;
pub mod settings;
pub mod update;
pub mod update_manifest;

#[cfg(windows)]
pub mod settings_slint;
