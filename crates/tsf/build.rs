//! TSF DLL 的两个构建期约束。
//!
//! 一、rustc 给 cdylib 的 `#[no_mangle]` 导出经 .drectve 传给链接器,MSVC 对此
//! 固定报 LNK4104(should be PRIVATE,rust-lang/rust#86431,无害);再手动
//! `/EXPORT:...,PRIVATE` 会与 drectve 撞成 LNK4197。直接按官方 /IGNORE
//! 白名单压制该编号。
//!
//! 二、TSF DLL 会被装进宿主进程(记事本、浏览器、AppContainer/UWP 宿主),
//! 宿主不保证能加载 vcruntime140.dll;缺了运行库,整个输入法就加载不上。
//! 所以 Windows 上的 TSF 必须静态链接 CRT。忘了加
//! `RUSTFLAGS="-C target-feature=+crt-static"` 时在这里立刻失败,而不是产出
//! 一个依赖系统运行库、到线上才发现加载失败的 DLL。

fn main() {
    println!("cargo:rustc-cdylib-link-arg=/IGNORE:4104");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    // 只有 Windows 产物会被装进宿主进程;Linux 上的编译(Clippy 门禁)不适用。
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if has_static_crt() {
        return;
    }
    panic!(
        "lueying-tsf 必须静态链接 CRT:用 \
         RUSTFLAGS=\"-C target-feature=+crt-static\" 构建(见 .cargo/config.toml 的说明)"
    );
}

/// 静态 CRT 的两个信号:目标特性(crt-static)或显式的 RUSTFLAGS。
/// 两个都认,免得 cargo 版本差异把正确的构建判成错的。
fn has_static_crt() -> bool {
    let features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    if features.split(',').any(|f| f == "crt-static") {
        return true;
    }
    let flags = std::env::var("RUSTFLAGS").unwrap_or_default()
        + &std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    flags
        .split(|c: char| c == ' ' || c == ',' || c == '"')
        .any(|f| f.contains("crt-static"))
}
