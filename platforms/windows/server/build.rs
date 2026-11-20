//! 嵌入 uiAccess 清单:候选窗要进宿主浮层的窗口带,清单是资格之一。
//! 另两个前提(Authenticode 信任链、安全安装位置)由安装器的自签名保证。
//!
//! 预览/调试构建设 `LUEYING_NO_UIACCESS=1` 跳过清单:无签名 + uiAccess
//! 会被进程创建直接拒绝,预览就不跑不了了。

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rerun-if-env-changed=LUEYING_NO_UIACCESS");
    if target.contains("windows") && std::env::var("LUEYING_NO_UIACCESS").is_err() {
        let mut resource = winresource::WindowsResource::new();
        resource.set_manifest_file("server.manifest");
        resource.compile().expect("嵌入 uiAccess 清单失败");
    }
}
