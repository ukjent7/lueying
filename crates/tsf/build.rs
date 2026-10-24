//! MSVC 链接器对 `#[no_mangle]` 导出的 cdylib 报 LNK4104
//! (export of symbol 'Dll*' should be PRIVATE)。显式以 PRIVATE 重新声明
//! 这四个导出消除告警,导出集合不变。
fn main() {
    for sym in [
        "DllCanUnloadNow",
        "DllGetClassObject",
        "DllRegisterServer",
        "DllUnregisterServer",
    ] {
        println!("cargo:rustc-cdylib-link-arg=/EXPORT:{sym}={sym},PRIVATE");
    }
}
