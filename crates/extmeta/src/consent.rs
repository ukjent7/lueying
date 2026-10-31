//! 扩展授权:能力说明书、包内容指纹与同意记录。
//!
//! 同意记录是宿主对扩展的信任锚:键是扩展 id,值里存包内容哈希与当时
//! 声明过的能力。加载与热重载都按哈希核对;清单、入口脚本或能力集合变了,
//! 扩展先停用,等用户在设置页重新同意。
//!
//! 记录文件在扩展目录根上的 `consent.json`。它只由宿主写;包目录里出现的
//! 同名文件不会被读取。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::sha256::{Sha256, hex};
use crate::{Manifest, Permission, Service};

/// 同意记录的格式版本。
pub const CONSENT_VERSION: u32 = 1;
/// 同意记录文件的最大读取量;它只该有几百字节。
const MAX_BOOK_BYTES: usize = 256 * 1024;
/// 哈希只读清单与入口脚本,入口脚本本身另有 512 KiB 上限。
const MAX_HASH_BYTES: usize = 1024 * 1024;
const DOMAIN: &[u8] = b"lueying-ext-package-v1\0";

/// 一个扩展的同意内容。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentEntry {
    /// 同意时的包内容哈希(清单 + 入口脚本)。
    pub hash: String,
    /// 同意时的扩展版本,只用于显示。
    pub version: String,
    /// 同意时的能力名。
    pub capabilities: Vec<String>,
    /// 同意时需要的宿主服务名。
    pub needs: Vec<String>,
    /// 同意时间,Unix 毫秒。
    pub approved_ms: u64,
}

/// `consent.json` 的内容。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentBook {
    /// 格式版本。
    #[serde(default = "version")]
    pub version: u32,
    /// 扩展 id → 同意内容。
    #[serde(default)]
    pub extensions: BTreeMap<String, ConsentEntry>,
}

impl Default for ConsentBook {
    fn default() -> Self {
        Self {
            version: CONSENT_VERSION,
            extensions: BTreeMap::new(),
        }
    }
}

fn version() -> u32 {
    CONSENT_VERSION
}

/// 一次同意检查的结果。`required: false` 的包(纯外观、不碰服务)总是可加载。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsentCheck {
    /// 是否需要同意。
    pub required: bool,
    /// 当前内容与能力是否已被同意。
    pub approved: bool,
    /// 曾同意过,但包内容或能力变了。
    pub changed: bool,
    /// 这次新增的能力/服务人话;首次安装时为空。
    pub added: Vec<String>,
    /// 全部能力/服务人话,供设置页与同意对话框展示。
    pub items: Vec<String>,
    /// 是否同时申请「读取用户内容」与「访问网络」。
    pub sensitive: bool,
}

impl ConsentBook {
    /// 读记录;文件缺失或损坏都按空记录处理。损坏时扩展会停用,
    /// 用户重新同意即可恢复,不会静默放行。
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        if bytes.len() > MAX_BOOK_BYTES {
            return Self::default();
        }
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// 原子写回:先写临时文件再改名,写一半不会留下坏记录。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("建扩展目录失败: {e}"))?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("序列化同意记录失败: {e}"))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("写同意记录失败: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("替换同意记录失败: {e}"))
    }

    /// 核对一个包;哈希读不出来时返回错误,调用方按「未同意」处理。
    pub fn check(&self, dir: &Path, manifest: &Manifest) -> Result<ConsentCheck, String> {
        let required = !manifest.capabilities.is_empty() || !manifest.needs.is_empty();
        if !required {
            return Ok(ConsentCheck {
                approved: true,
                ..ConsentCheck::default()
            });
        }
        let hash = package_hash(dir, manifest)?;
        let entry = self.extensions.get(&manifest.id);
        let mut check = ConsentCheck {
            required: true,
            approved: entry.is_some_and(|entry| entry.hash == hash),
            changed: entry.is_some_and(|entry| entry.hash != hash),
            added: Vec::new(),
            items: consent_items(manifest),
            sensitive: consent_sensitive(manifest),
        };
        if let Some(entry) = entry
            && entry.hash != hash
        {
            let old: Vec<&String> = entry
                .capabilities
                .iter()
                .chain(entry.needs.iter())
                .collect();
            for key in requested(manifest) {
                if !old.iter().any(|known| **known == key) {
                    push_unique(&mut check.added, label(&key));
                }
            }
        }
        Ok(check)
    }

    /// 记下一个包的当前内容与能力;调用方负责 [`Self::save`]。
    pub fn approve(
        &mut self,
        dir: &Path,
        manifest: &Manifest,
        now_ms: u64,
    ) -> Result<ConsentEntry, String> {
        let entry = ConsentEntry {
            hash: package_hash(dir, manifest)?,
            version: manifest.version.clone(),
            capabilities: manifest.capabilities.clone(),
            needs: manifest
                .needs
                .iter()
                .map(|service| service.key().to_string())
                .collect(),
            approved_ms: now_ms,
        };
        self.extensions.insert(manifest.id.clone(), entry.clone());
        Ok(entry)
    }
}

/// 包内容哈希:清单与入口脚本按固定框架串起来;其它文件不参与。
///
/// 运行期只读这两个文件,把别的数据文件算进去只会让无关改动触发重同意。
pub fn package_hash(dir: &Path, manifest: &Manifest) -> Result<String, String> {
    let main = manifest
        .script_main()
        .ok_or_else(|| format!("扩展 {} 没有脚本入口", manifest.id))?;
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    for name in ["manifest.json", main] {
        let bytes = read_capped(&dir.join(name))?;
        hasher.update(&(name.len() as u32).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&(bytes.len() as u64).to_be_bytes());
        hasher.update(&bytes);
    }
    Ok(hex(&hasher.finish()))
}

/// 授权页上的人话能力表。能力和服务重复时只留能力那一行。
pub fn consent_items(manifest: &Manifest) -> Vec<String> {
    let mut items = Vec::new();
    for capability in &manifest.capabilities {
        if let Some(permission) = Permission::ALL
            .into_iter()
            .find(|permission| permission.key() == capability)
        {
            push_unique(&mut items, permission.title().to_string());
        }
    }
    for service in &manifest.needs {
        if let Some(permission) = service.permission()
            && manifest
                .capabilities
                .iter()
                .any(|capability| capability == permission.key())
        {
            continue;
        }
        push_unique(&mut items, service.title().to_string());
    }
    items
}

/// 「读取用户内容 + 外发」组合单独强提示。
pub fn consent_sensitive(manifest: &Manifest) -> bool {
    let network = manifest.capabilities.iter().any(|key| key == "network")
        || manifest.needs.contains(&Service::Net);
    let reads = manifest.capabilities.iter().any(|key| {
        Permission::ALL
            .into_iter()
            .find(|permission| permission.key() == key)
            .is_some_and(Permission::reads_user_content)
    }) || manifest.needs.iter().any(|service| {
        matches!(
            *service,
            Service::Capture | Service::Ocr | Service::Clipboard
        )
    });
    network && reads
}

/// 清单声明的全部键,按能力后服务的顺序。
fn requested(manifest: &Manifest) -> Vec<String> {
    manifest
        .capabilities
        .iter()
        .cloned()
        .chain(
            manifest
                .needs
                .iter()
                .map(|service| service.key().to_string()),
        )
        .collect()
}

/// 键转人话;未知键原样显示,方便发现清单与模型不同步。
fn label(key: &str) -> String {
    if let Some(permission) = Permission::ALL
        .into_iter()
        .find(|permission| permission.key() == key)
    {
        return permission.title().to_string();
    }
    if let Some(service) = Service::ALL
        .into_iter()
        .find(|service| service.key() == key)
    {
        return service.title().to_string();
    }
    key.to_string()
}

fn push_unique(items: &mut Vec<String>, item: String) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn read_capped(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    if meta.len() > MAX_HASH_BYTES as u64 {
        return Err(format!("{} 超过 {MAX_HASH_BYTES} 字节", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    if bytes.len() > MAX_HASH_BYTES {
        return Err(format!("{} 超过 {MAX_HASH_BYTES} 字节", path.display()));
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "consent_tests.rs"]
mod tests;
