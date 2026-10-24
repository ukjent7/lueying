//! rustc 给 cdylib 的 `#[no_mangle]` 导出经 .drectve 传给链接器,MSVC 对此
//! 固定报 LNK4104(should be PRIVATE,rust-lang/rust#86431,无害);再手动
//! `/EXPORT:...,PRIVATE` 会与 drectve 撞成 LNK4197。直接按官方 /IGNORE
//! 白名单压制该编号。
fn main() {
    println!("cargo:rustc-cdylib-link-arg=/IGNORE:4104");
}
