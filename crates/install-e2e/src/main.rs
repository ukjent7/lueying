//! Windows 安装/卸载冒烟测试:用发布目录编译真实 Inno Setup 安装包,指向隔离
//! 目录静默安装,检查 x64/x86 文件、自启动/卸载项与两套 TSF 注册;再用真实
//! 更新器升级到 9.9.9,最后运行卸载器确认清理。产物 `target/install-e2e-report.md`。

fn main() {
    #[cfg(windows)]
    {
        imp::run();
    }
    #[cfg(not(windows))]
    println!("lueying-install-e2e 仅支持 Windows");
}

#[cfg(windows)]
#[path = "imp.rs"]
mod imp;
