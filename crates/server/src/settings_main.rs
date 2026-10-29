//! 设置窗独立进程:输入方案、候选外观、标点、词库数据与更新。
//!
//! 窗口是原生 Win32 窗,画面走 Skia 光栅;页面模型在
//! `lueying_server::settings_view`,平台侧动作在 `settings_actions`。
//! 保存写 `config.ini`,再经命名管道让常驻 Server 立即重载。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    lueying_server::settings_win::run();
    #[cfg(not(windows))]
    println!("lueying-settings 仅支持 Windows");
}
