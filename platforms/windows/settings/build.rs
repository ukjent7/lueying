//! Slint 编译:settings.slint 生成 Rust 组件,只在 Windows 目标参与。
fn main() {
    // 交叉编译时 build.rs 跑在 Linux 主机,`cfg(windows)` 说的是主机;
    // 目标平台要看 Cargo 注入的 CARGO_CFG_TARGET_OS。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        slint_build::compile("ui/settings.slint").expect("settings.slint 编译失败");
    }
}
