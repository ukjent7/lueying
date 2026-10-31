//! lueying-server:常驻服务进程。持有词典(一份)与每个宿主线程一个引擎会话,
//! 经命名管道接收 TSF DLL 的按键请求并返回组句/上屏。候选窗 UI 随后接入。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        // 命令面板自检:独立进程建窗、注入按键、验证焦点归还,不碰管道。
        if std::env::args().nth(1).as_deref() == Some("--panel-selftest") {
            imp::panel_selftest();
            return;
        }
        // Skia GPU 自检的子进程:只跑 GPU 自检,跑完就退,不碰管道与单实例锁。
        // GPU 管线在驱动异常的机器上会崩,隔离在子进程里,崩了也不影响服务端。
        if std::env::var_os("LUEYING_SKIA_SELFTEST_GPU").is_some() {
            ui_skia_selftest::gpu_selftest();
            return;
        }
        // V3 帧级基准的 GPU 子进程:同一条崩溃隔离;结果打到 stdout 给父进程。
        if std::env::var_os("LUEYING_SKIA_BENCH_GPU").is_some() {
            ui_skia_bench::gpu_bench();
            return;
        }
        imp::real_main();
    }
    #[cfg(not(windows))]
    println!("lueying-server 仅支持 Windows");
}

#[cfg(windows)]
mod ext_audit;

#[cfg(windows)]
mod ext_host;

#[cfg(windows)]
mod ext_selftest;

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
mod ui_panel;

#[cfg(windows)]
mod ui_place;

#[cfg(windows)]
mod ui_skia;

#[cfg(windows)]
mod ui_skia_cpu;

#[cfg(windows)]
mod ui_skia_bench;

#[cfg(windows)]
mod ui_skia_effect;

#[cfg(windows)]
mod ui_skia_gpu;

#[cfg(windows)]
mod ui_skia_mouse;

#[cfg(windows)]
mod ui_skia_script;

#[cfg(windows)]
mod ui_skia_selftest;

#[cfg(windows)]
mod user_store;
