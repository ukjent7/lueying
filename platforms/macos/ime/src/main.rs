//! 掠影 macOS 输入法外壳。
//!
//! 非 macOS 目标保留一个空入口,让整个工作区在 Linux/Windows 上照常
//! 运行 fmt 与 Clippy;真正的 InputMethodKit 代码只在 macOS 编译。

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod canvas;
#[cfg(target_os = "macos")]
mod candidate;
#[cfg(target_os = "macos")]
mod controller;
#[cfg(target_os = "macos")]
mod session;
#[cfg(target_os = "macos")]
mod theme;

fn main() {
    #[cfg(target_os = "macos")]
    app::run();

    #[cfg(not(target_os = "macos"))]
    eprintln!("lueying-macos 只能在 macOS 上运行");
}
