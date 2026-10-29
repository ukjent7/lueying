//! lueying-server:常驻服务进程。持有词典(一份)与每个宿主线程一个引擎会话,
//! 经命名管道接收 TSF DLL 的按键请求并返回组句/上屏。候选窗 UI 随后接入。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        // Skia GPU 自检的子进程:只跑 GPU 自检,跑完就退,不碰管道与单实例锁。
        // GPU 管线在驱动异常的机器上会崩,隔离在子进程里,崩了也不影响服务端。
        if std::env::var_os("LUEYING_SKIA_SELFTEST_GPU").is_some() {
            ui_skia_selftest::gpu_selftest();
            return;
        }
        imp::real_main();
    }
    #[cfg(not(windows))]
    println!("lueying-server 仅支持 Windows");
}

#[cfg(windows)]
mod imp;

#[cfg(windows)]
mod modules;

#[cfg(windows)]
mod pipe;

#[cfg(windows)]
mod settle;

#[cfg(windows)]
mod uia;

#[cfg(windows)]
mod ui;

#[cfg(windows)]
mod ui_hud;

#[cfg(windows)]
mod ui_place;

#[cfg(windows)]
mod ui_skia;

#[cfg(windows)]
mod ui_skia_cpu;

#[cfg(windows)]
mod ui_skia_effect;

#[cfg(windows)]
mod ui_skia_gpu;

#[cfg(windows)]
mod ui_skia_selftest;

#[cfg(windows)]
mod user_store;
