//! lueying-server:常驻服务进程。持有词典(一份)与每个宿主线程一个引擎会话,
//! 经命名管道接收 TSF DLL 的按键请求并返回组句/上屏。候选窗 UI 随后接入。

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        // 选择面板自检:独立进程建窗、注入按键、验证焦点归还,不碰管道。
        if std::env::args().nth(1).as_deref() == Some("--panel-selftest") {
            imp::panel_selftest();
            return;
        }
        // 玻璃预览:亮/暗合成背景上叠玻璃帧,存 BMP 供人眼核对。
        if std::env::args().nth(1).as_deref() == Some("--glass-preview") {
            let prefix = std::env::args()
                .nth(2)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("target/glass"));
            crate::ui::d2d::selftest::glass_preview(&prefix);
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
#[cfg(windows)]
#[cfg(windows)]
#[cfg(windows)]
mod imp;

#[cfg(windows)]
mod modules;

#[cfg(windows)]
mod pipe;

#[cfg(windows)]
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
