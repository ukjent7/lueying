//! 签名更新清单:生成 `lueying-manifest.json` 并用 Ed25519 种子签名。
//!
//! `main` 的 `#[path]` 子模块(src 保持扁平)。

use std::path::PathBuf;

pub(crate) struct UpdateManifestArgs {
    version: String,
    name: String,
    size: u64,
    sha256: String,
    mirrors: Vec<String>,
    out: PathBuf,
}

pub(crate) fn parse_update_manifest(it: &mut impl Iterator<Item = String>) -> UpdateManifestArgs {
    let mut args = UpdateManifestArgs {
        version: env!("CARGO_PKG_VERSION").to_string(),
        name: String::new(),
        size: 0,
        sha256: String::new(),
        mirrors: Vec::new(),
        out: PathBuf::from("dist"),
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => args.version = it.next().unwrap_or(args.version),
            "--name" => args.name = it.next().unwrap_or(args.name),
            "--size" => {
                args.size = it
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(args.size);
            }
            "--sha256" => args.sha256 = it.next().unwrap_or(args.sha256),
            "--mirror" => {
                if let Some(url) = it.next() {
                    args.mirrors.push(url);
                }
            }
            "--out" => args.out = it.next().map(PathBuf::from).unwrap_or(args.out),
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

pub(crate) struct SignArgs {
    manifest: PathBuf,
    key: Option<PathBuf>,
    key_env: Option<String>,
}

pub(crate) fn parse_sign_manifest(it: &mut impl Iterator<Item = String>) -> SignArgs {
    let mut args = SignArgs {
        manifest: PathBuf::from("dist/lueying-manifest.json"),
        key: None,
        key_env: None,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--manifest" => args.manifest = it.next().map(PathBuf::from).unwrap_or(args.manifest),
            "--key" => args.key = it.next().map(PathBuf::from),
            "--key-env" => args.key_env = it.next(),
            other => {
                eprintln!("未知参数: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

/// 生成签名更新清单:只描述这一个安装包的大小、哈希与镜像;发布说明仍以
/// GitHub Release 正文为准,清单里不重复。
pub(crate) fn update_manifest(args: UpdateManifestArgs) -> Result<(), String> {
    if !is_sha256_hex(&args.sha256) {
        return Err("--sha256 需要 64 位十六进制".into());
    }
    if args.mirrors.is_empty() {
        return Err("--mirror 至少一条下载地址".into());
    }
    if args.mirrors.iter().any(|url| !url.starts_with("https://")) {
        return Err("--mirror 只接受 https 地址".into());
    }
    let manifest = serde_json::json!({
        "version": 1,
        "release": args.version,
        "files": [{
            "name": args.name,
            "size": args.size,
            "sha256": args.sha256,
            "mirrors": args.mirrors,
        }],
    });
    let mut text = serde_json::to_string_pretty(&manifest).map_err(|e| format!("生成清单: {e}"))?;
    text.push('\n');
    let path = args.out.join("lueying-manifest.json");
    std::fs::write(&path, &text).map_err(|e| format!("写 {}: {e}", path.display()))?;
    println!(
        "清单 {} 已生成;用 sign-manifest 签名后随 Release 上传",
        path.display()
    );
    Ok(())
}

/// 用 Ed25519 种子给清单签名,输出 `.sig`(128 位十六进制,清单字节原文的
/// 签名);签完立即用内置流程自检,并回显公钥指纹供人工与代码常量核对。
pub(crate) fn sign_manifest(args: SignArgs) -> Result<(), String> {
    use ed25519_dalek::{Signer, SigningKey, Verifier};
    let seed_hex = match (&args.key, &args.key_env) {
        (Some(path), _) => {
            std::fs::read_to_string(path).map_err(|e| format!("读 {}: {e}", path.display()))?
        }
        (None, Some(name)) => std::env::var(name).map_err(|_| format!("环境变量 {name} 未设置"))?,
        (None, None) => {
            return Err("用 --key <种子文件> 或 --key-env <变量名> 提供签名种子".into());
        }
    };
    let seed = decode_seed(seed_hex.trim())?;
    let key = SigningKey::from_bytes(&seed);
    let bytes = std::fs::read(&args.manifest)
        .map_err(|e| format!("读 {}: {e}", args.manifest.display()))?;
    let signature = key.sign(&bytes);
    key.verifying_key()
        .verify(&bytes, &signature)
        .map_err(|e| format!("签名自检失败: {e}"))?;
    let name = args
        .manifest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("lueying-manifest.json");
    let sig_path = args.manifest.with_file_name(format!("{name}.sig"));
    let mut text = hex(&signature.to_bytes());
    text.push('\n');
    std::fs::write(&sig_path, &text).map_err(|e| format!("写 {}: {e}", sig_path.display()))?;
    println!(
        "签名 {} 已生成;公钥指纹 {}",
        sig_path.display(),
        hex(&key.verifying_key().to_bytes())
    );
    Ok(())
}

fn decode_seed(text: &str) -> Result<[u8; 32], String> {
    if text.len() != 64 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("签名种子要 64 位十六进制(Ed25519 私钥种子)".into());
    }
    let mut seed = [0u8; 32];
    for (index, slot) in seed.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| "签名种子要 64 位十六进制".to_string())?;
    }
    Ok(seed)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}
