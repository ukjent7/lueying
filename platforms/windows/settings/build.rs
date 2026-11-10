//! Slint 编译:settings.slint 生成 Rust 组件,只在 Windows 目标参与。
fn main() {
    // slint-build 是目标限定的 build-dependency,Linux 上的 Clippy 拿不到它。
    #[cfg(windows)]
    slint_build::compile("ui/settings.slint").expect("settings.slint 编译失败");
}
