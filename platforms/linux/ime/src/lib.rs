//! 掠影 Linux 输入法外壳。
//!
//! Wayland 的 `zwp_input_method_v2` 在输入法进程里跑完整引擎:硬件键盘由
//! 键盘 grab 接管,组合与候选窗走 `zwp_input_popup_surface_v2`,未消费的
//! 按键经虚拟键盘原样送回客户端。候选窗由 `lueying-render` 描述、用
//! `tiny-skia` 软件绘制到 shm 缓冲,不依赖 GTK/Qt。

#[cfg(target_os = "linux")]
pub mod config;
#[cfg(target_os = "linux")]
pub mod keymap;
#[cfg(target_os = "linux")]
pub mod model;
#[cfg(target_os = "linux")]
pub mod session;
#[cfg(target_os = "linux")]
pub mod theme;
#[cfg(target_os = "linux")]
pub mod ui;
#[cfg(target_os = "linux")]
pub mod wayland;

/// 启动输入法。非 Linux 目标只保留一个可编译的空入口。
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    {
        wayland::run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(std::io::Error::other("lueying-linux 只能在 Linux 上运行").into())
    }
}
