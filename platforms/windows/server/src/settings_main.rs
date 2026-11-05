//! 设置窗独立进程:输入方案、候选外观、标点、词库数据与更新。
//!
//! 窗口是 Slint(winit 后端 + femtovg GPU 渲染,无头环境回退软件渲染);页面模型在
//! `lueying_server::settings::view`,宿主接线在 `settings_slint`,
//! 平台侧动作在 `settings::actions`。保存写 `config.toml`,再经命名
//! 管道让常驻 Server 立即重载。
//!
//! `--selftest <报告路径>` 跑一遍自检:模型五页铺得开、关键控件在位、
//! Slint 组件能实例化,Windows CI 用它确认设置窗这条链路没断。
//! `--dump-config-schema` 输出设置 schema 的 JSON,供脚本与 AI agent 读取。

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod settings_slint;

fn main() {
    // 平台无关:脚本与 AI agent 读取设置元数据,不需要启动窗口。
    if std::env::args().nth(1).as_deref() == Some("--dump-config-schema") {
        println!("{}", lueying_server::settings::schema::config_schema_text());
        return;
    }
    #[cfg(windows)]
    {
        let mut args = std::env::args().skip(1);
        if args.next().as_deref() == Some("--selftest") {
            let path = args
                .next()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("lueying-settings-selftest.txt"));
            if let Err(e) = settings_slint::selftest(&path) {
                eprintln!("设置窗自检失败: {e}");
                std::process::exit(1);
            }
            return;
        }
        settings_slint::run();
    }
    #[cfg(not(windows))]
    println!("lueying-settings 仅支持 Windows");
}
