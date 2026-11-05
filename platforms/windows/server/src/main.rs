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
        imp::real_main();
    }
    #[cfg(not(windows))]
    println!("lueying-server 仅支持 Windows");
}

#[cfg(windows)]
mod context;

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
mod candidate_events;
#[cfg(windows)]
mod settle;

#[cfg(windows)]
mod sync_rerank;

#[cfg(windows)]
mod uia;

#[cfg(windows)]
mod ui;

#[cfg(windows)]
mod user_store;
