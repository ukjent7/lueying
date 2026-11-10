//! 签名更新清单:ed25519 内置公钥验签,替代「只防损坏」的 `.sha256` 文件。
//!
//! 清单 [`MANIFEST_NAME`] 列出安装包的名称、大小、SHA-256 与多镜像下载地址;
//! 签名文件是清单字节原文的 Ed25519 签名,128 位小写十六进制。客户端先验签
//! 再信内容:清单损坏、签名不符或公钥不认识都验不过,更新就地停止,不退回
//! 未签名的旧流程。发布私钥保存在 GitHub Secret(`LUEYING_SIGN_KEY`,64 位
//! 十六进制种子),由 `cargo xtask sign-manifest` 在发布工作流里签名;换发布
//! 密钥时同步更新 [`PUBLIC_KEY`] 与 secret。

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;

/// 清单在 Release 资源里的固定名字。
pub const MANIFEST_NAME: &str = "lueying-manifest.json";

/// 清单大小上限;清单只放元数据,超了说明放错东西。
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// 内置发布公钥;与 `~/.config/lueying-sign` 的种子配对,CI 里 `sign-manifest`
/// 签完会回显同一指纹,便于人工核对。
pub const PUBLIC_KEY: [u8; 32] = [
    0x88, 0xed, 0xa0, 0xc1, 0xe5, 0x5b, 0x03, 0xe6, 0x8f, 0xdd, 0x50, 0x9f, 0x64, 0x0a, 0x93, 0x7d,
    0xe2, 0x96, 0xe8, 0xb1, 0x02, 0xf5, 0xba, 0xaf, 0xba, 0x47, 0x8b, 0xd6, 0xf5, 0x5f, 0x04, 0xeb,
];

/// 签名更新清单;字段与发布工作流生成的 `lueying-manifest.json` 对应。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct UpdateManifest {
    /// 清单格式版本;当前只有 1。
    #[serde(default)]
    pub version: u32,
    /// 发布版本号,与安装包文件名里的版本一致。
    pub release: String,
    /// 发布说明;客户端展示仍以 GitHub Release 正文为准,这里留空即可。
    #[serde(default)]
    pub notes: String,
    /// 清单里的全部文件;至少一条,名字唯一。
    pub files: Vec<ManifestFile>,
}

/// 清单里的一个文件:大小、哈希与按序尝试的镜像。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    /// 文件名;不允许路径分隔符。
    pub name: String,
    /// 字节数;0 表示不校验大小。
    pub size: u64,
    /// SHA-256,64 位小写十六进制。
    pub sha256: String,
    /// 下载地址;全部 https,1 到 8 条。当前实现取首条,其余为多源预留。
    pub mirrors: Vec<String>,
}

/// 解析并校验清单;不验签名。字段越界、名字带路径、镜像非 https 都报可读原因。
pub fn parse(bytes: &[u8]) -> Result<UpdateManifest, String> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(format!(
            "清单超过 {MAX_MANIFEST_BYTES} 字节(实际 {})",
            bytes.len()
        ));
    }
    let manifest: UpdateManifest =
        serde_json::from_slice(bytes).map_err(|e| format!("清单解析失败: {e}"))?;
    if manifest.version != 1 {
        return Err(format!("清单版本 {} 不认识,当前只支持 1", manifest.version));
    }
    if manifest.release.is_empty() {
        return Err("清单缺少 release 版本号".into());
    }
    if manifest.files.is_empty() {
        return Err("清单里没有文件".into());
    }
    if manifest.files.len() > 16 {
        return Err(format!("清单最多 16 个文件(实际 {})", manifest.files.len()));
    }
    for (index, file) in manifest.files.iter().enumerate() {
        file.validate()?;
        if manifest.files[..index]
            .iter()
            .any(|other| other.name.eq_ignore_ascii_case(&file.name))
        {
            return Err(format!("文件 {} 重复声明", file.name));
        }
    }
    Ok(manifest)
}

impl ManifestFile {
    fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || self.name.contains('/')
            || self.name.contains('\\')
            || self.name.contains("..")
        {
            return Err(format!("文件名 {:?} 不合法", self.name));
        }
        if !is_sha256_hex(&self.sha256) {
            return Err(format!("文件 {} 的 SHA-256 不是 64 位十六进制", self.name));
        }
        if self.mirrors.is_empty() {
            return Err(format!("文件 {} 没有镜像地址", self.name));
        }
        if self.mirrors.len() > 8 {
            return Err(format!(
                "文件 {} 的镜像最多 8 条(实际 {})",
                self.name,
                self.mirrors.len()
            ));
        }
        for mirror in &self.mirrors {
            if !mirror.starts_with("https://") || mirror.chars().any(|c| c.is_whitespace()) {
                return Err(format!(
                    "文件 {} 的镜像 {:?} 不是 https 地址",
                    self.name, mirror
                ));
            }
        }
        Ok(())
    }

    /// 按序取第一个可用镜像;清单保证至少一条 https。
    pub fn mirror(&self) -> Result<&str, String> {
        self.mirrors
            .first()
            .map(String::as_str)
            .ok_or_else(|| format!("文件 {} 没有镜像地址", self.name))
    }

    /// 名字是否匹配(忽略大小写);客户端按安装包命名找条目。
    pub fn matches(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

impl UpdateManifest {
    /// 找安装包条目;按掠影安装包命名 `lueying-<版本>-setup.exe` 匹配。
    pub fn installer(&self, version: &str) -> Result<&ManifestFile, String> {
        let setup = format!("lueying-{version}-setup.exe");
        self.files
            .iter()
            .find(|file| file.matches(&setup))
            .ok_or_else(|| format!("清单里没有安装包 {setup}"))
    }
}

/// 验签并解析:先对清单字节原文验 Ed25519 签名,再走 [`parse`] 的字段校验。
/// 签名文件是 128 位小写或大写十六进制。
pub fn verify(bytes: &[u8], signature_hex: &str) -> Result<UpdateManifest, String> {
    let signature = decode_signature(signature_hex)?;
    // 内置公钥是编译期常量,格式错误属于代码错误,panic 即暴露。
    let key = VerifyingKey::from_bytes(&PUBLIC_KEY).expect("内置公钥必须是合法 Ed25519 公钥");
    key.verify(bytes, &signature)
        .map_err(|_| "清单签名验证失败:公钥不匹配或清单被改动".to_string())?;
    parse(bytes)
}

fn decode_signature(signature_hex: &str) -> Result<Signature, String> {
    let text = signature_hex.trim();
    if text.len() != 128 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("签名文件不是 128 位十六进制".into());
    }
    let mut bytes = [0u8; 64];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| "签名文件不是 128 位十六进制")?;
    }
    Ok(Signature::from_bytes(&bytes))
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "update_manifest_tests.rs"]
mod tests;
