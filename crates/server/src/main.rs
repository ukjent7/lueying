//! lueying-server:常驻服务进程。持有词典(一份)与每个宿主线程一个引擎会话,
//! 经命名管道接收 TSF DLL 的按键请求并返回组句/上屏。候选窗 UI 随后接入。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    imp::real_main();
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
mod ui_place;

#[cfg(windows)]
mod ui_skia;

#[cfg(windows)]
mod ui_skia_effect;

#[cfg(windows)]
mod ui_skia_gpu;

#[cfg(windows)]
mod ui_skia_selftest;

#[cfg(windows)]
mod user_store;
