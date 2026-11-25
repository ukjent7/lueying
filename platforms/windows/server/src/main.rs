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
        // 调试快照:独立进程渲染真实选择面板,不碰管道、单实例锁与 TSF。
        #[cfg(debug_assertions)]
        if std::env::args().nth(1).as_deref() == Some("--visual-picker") {
            let mut args = std::env::args().skip(2);
            let appearance = match args.next().as_deref() {
                Some("light") => lueying_server::theme::Appearance::Light,
                Some("dark") => lueying_server::theme::Appearance::Dark,
                Some("contrast") => lueying_server::theme::Appearance::HighContrast,
                _ => std::process::exit(2),
            };
            let Some(output) = args.next() else {
                std::process::exit(2);
            };
            let result = crate::ui::picker::snap(appearance, output);
            if let Err(error) = result {
                eprintln!("{error}");
                std::process::exit(1);
            }
            return;
        }
        // 调试快照:独立进程渲染真实工具栏,不碰管道、单实例锁与 TSF。
        #[cfg(debug_assertions)]
        if std::env::args().nth(1).as_deref() == Some("--visual-toolbar") {
            let mut args = std::env::args().skip(2);
            let scene = match args.next().as_deref() {
                Some("ceramic") => "ceramic",
                Some("glass-dark") => "glass-dark",
                Some("glass-light") => "glass-light",
                _ => std::process::exit(2),
            };
            let Some(scale) = args.next().and_then(|arg| arg.parse::<f32>().ok()) else {
                std::process::exit(2);
            };
            let range = lueying_render::toolbar::SCALE_MIN..=lueying_render::toolbar::SCALE_MAX;
            if !scale.is_finite() || !range.contains(&scale) {
                std::process::exit(2);
            }
            let Some(output) = args.next() else {
                std::process::exit(2);
            };
            let result = crate::ui::toolbar::snap(scene, scale, output);
            if let Err(error) = result {
                eprintln!("{error}");
                std::process::exit(1);
            }
            return;
        }
        // 调试快照:独立进程渲染真实候选窗,不碰管道、单实例锁与 TSF。
        #[cfg(debug_assertions)]
        if std::env::args().nth(1).as_deref() == Some("--visual-candidate") {
            let mut args = std::env::args().skip(2);
            let scene = match args.next().as_deref() {
                Some("strip") => "strip",
                Some("grid") => "grid",
                Some("strip-contrast") => "strip-contrast",
                Some("grid-contrast") => "grid-contrast",
                Some("column-stress") => "column-stress",
                _ => std::process::exit(2),
            };
            let Some(output) = args.next() else {
                std::process::exit(2);
            };
            let result = crate::ui::rune::snap(scene, output);
            if let Err(error) = result {
                eprintln!("{error}");
                std::process::exit(1);
            }
            return;
        }
        #[cfg(debug_assertions)]
        if std::env::args().nth(1).as_deref() == Some("--visual-candidate-motion") {
            let mut args = std::env::args().skip(2);
            let (reduce, glass_up, moving, flight) = match args.next().as_deref() {
                Some("normal") => (false, false, false, false),
                Some("reduced") => (true, false, false, false),
                Some("glass-up") => (false, true, false, false),
                Some("glass-up-reduced") => (true, true, false, false),
                Some("glass-up-live") => (false, true, true, false),
                Some("flight") => (false, false, false, true),
                _ => std::process::exit(2),
            };
            let Some(output) = args.next() else {
                std::process::exit(2);
            };
            let Some(frames) = args.next() else {
                std::process::exit(2);
            };
            let result = crate::ui::rune::snap_motion(reduce, glass_up, moving, flight, output, frames);
            if let Err(error) = result {
                eprintln!("{error}");
                std::process::exit(1);
            }
            return;
        }
        imp::real_main();
    }
    #[cfg(not(windows))]
    println!("lueying-server 仅支持 Windows");
}

// 液体玻璃的背板光学:纯函数,非门控宿主也能跑单测。

// 候选窗选中胶囊的物理运动:纯模型,Windows 与单测共用。
#[cfg(any(windows, test))]

#[cfg(windows)]
mod context;

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
