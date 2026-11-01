//! Slint 编译:settings.slint 生成 Rust 组件,只在 Windows 目标参与。
fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        slint_build::compile("ui/settings.slint").expect("settings.slint 编译失败");
    }
}
