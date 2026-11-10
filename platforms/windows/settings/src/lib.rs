//! lueying-settings 的库部分:设置页模型、动作、备份与词库导入的文本格式。
//!
//! 页面模型不碰 Windows,Linux 上直接跑单测;Slint 窗口与平台动作只在 Windows 编译。
//! 进程入口在 `main.rs`。设置窗是独立进程,它是本产品唯一经 WinHTTP 联网的常驻界面
//! (更新、扩展商店);常驻 Server 不依赖本 crate。

pub mod bundle;
pub mod dict_import;
pub mod ext_secret;
pub mod settings;

#[cfg(windows)]
pub mod settings_slint;
