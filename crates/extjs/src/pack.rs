//! 扩展包:清单 + 入口脚本 + 热重载。
//!
//! 一个包是一个目录:`manifest.json` 与入口脚本(默认 `index.js`)。目录名
//! 必须等于清单里的 id,避免两个包抢同一个配置键。加载失败返回可读原因;
//! 热重载保留上一版可用的宿主,写坏的存盘不会把正在用的扩展弄没。

use std::path::{Path, PathBuf};

use crate::MAX_FAULTS;
use crate::host::{Host, Limits};
use crate::manifest::{MAX_MANIFEST_BYTES, MAX_SOURCE_BYTES, Manifest};
use crate::scene::{SceneIn, ScenePatch};

const MANIFEST: &str = "manifest.json";

/// 轮询结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    /// 文件没变。
    Unchanged,
    /// 已换到新版本。
    Reloaded,
    /// 文件变了但加载失败;上一版继续用。
    Failed(String),
    /// 清单被删了,包应当卸载。
    Removed,
}

/// 文件指纹:清单与入口脚本各一个内容哈希。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    manifest: u64,
    source: u64,
}

/// 一个已加载的扩展包。
pub struct Pack {
    dir: PathBuf,
    manifest: Manifest,
    stamp: Stamp,
    host: Host,
    limits: Limits,
    faults: u32,
    disabled: Option<String>,
    last_error: Option<String>,
    removed: bool,
}

impl Pack {
    /// 用默认上限加载一个包目录。
    pub fn load(dir: &Path) -> Result<Self, String> {
        Self::load_with(dir, Limits::default())
    }

    /// 用指定上限加载一个包目录。
    pub fn load_with(dir: &Path, limits: Limits) -> Result<Self, String> {
        let manifest_bytes = read_capped(&dir.join(MANIFEST), MAX_MANIFEST_BYTES)
            .map_err(|e| format!("读扩展清单失败: {e}"))?;
        let manifest = Manifest::parse(&manifest_bytes)?;
        check_dir(dir, &manifest)?;
        let main = manifest
            .appearance_main()
            .ok_or_else(|| format!("扩展 {} 没有外观组件", manifest.id))?;
        let source = read_text(&dir.join(main), MAX_SOURCE_BYTES)
            .map_err(|e| format!("读入口脚本 {main} 失败: {e}"))?;
        let host = Host::new(&source, limits)
            .map_err(|e| format!("扩展 {} 加载失败: {e}", manifest.id))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            stamp: Stamp {
                manifest: hash(&manifest_bytes),
                source: hash(source.as_bytes()),
            },
            manifest,
            host,
            limits,
            faults: 0,
            disabled: None,
            last_error: None,
            removed: false,
        })
    }

    /// 清单;设置页与日志用。
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// 包目录。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 最近一次失败的原因;成功后清空。
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// 连续失败被停用的原因;重载成功后清空。
    pub fn disabled_reason(&self) -> Option<&str> {
        self.disabled.as_deref()
    }

    /// 派发一次场景状态。
    ///
    /// 隐私字段直接返回 `None`,脚本不会被调用。返回的补丁在高对比度下
    /// 已摘掉效果,只留主题色。
    pub fn dispatch(&mut self, state: &SceneIn) -> Option<ScenePatch> {
        if state.private || self.removed || self.disabled.is_some() {
            return None;
        }
        let json = match state.to_json() {
            Ok(json) => json,
            Err(e) => {
                self.fault(e);
                return None;
            }
        };
        match self.host.dispatch(&json) {
            Ok(text) if text.is_empty() => {
                self.faults = 0;
                self.last_error = None;
                None
            }
            Ok(text) => match ScenePatch::parse(&text) {
                Ok(mut patch) => {
                    self.faults = 0;
                    self.last_error = None;
                    if state.high_contrast {
                        patch.effects = None;
                    }
                    Some(patch)
                }
                Err(e) => {
                    self.fault(e);
                    None
                }
            },
            Err(e) => {
                self.fault(e);
                None
            }
        }
    }

    /// 轮询文件变化;每次调用最多读两份小文件,由调用方控制频率。
    pub fn poll(&mut self) -> Poll {
        if self.removed {
            return Poll::Removed;
        }
        let manifest_bytes = match std::fs::read(self.dir.join(MANIFEST)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.removed = true;
                return Poll::Removed;
            }
            Err(e) => return Poll::Failed(format!("读扩展清单失败: {e}")),
        };
        let manifest_hash = hash(&manifest_bytes);
        let manifest = match Manifest::parse(&manifest_bytes) {
            Ok(manifest) => manifest,
            Err(e) => return self.stale(manifest_hash, e),
        };
        if let Err(e) = check_dir(&self.dir, &manifest) {
            return self.stale(manifest_hash, e);
        }
        let Some(main) = manifest.appearance_main().map(str::to_string) else {
            return self.stale(manifest_hash, format!("扩展 {} 没有外观组件", manifest.id));
        };
        let bytes = match read_capped(&self.dir.join(&main), MAX_SOURCE_BYTES) {
            Ok(bytes) => bytes,
            Err(e) => {
                return self.stale(manifest_hash, format!("读入口脚本 {main} 失败: {e}"));
            }
        };
        let source_hash = hash(&bytes);
        if manifest_hash == self.stamp.manifest && source_hash == self.stamp.source {
            return Poll::Unchanged;
        }
        let source = match String::from_utf8(bytes) {
            Ok(source) => source,
            Err(e) => {
                self.stamp = Stamp {
                    manifest: manifest_hash,
                    source: source_hash,
                };
                return Poll::Failed(format!("入口脚本不是 UTF-8: {e}"));
            }
        };
        match Host::new(&source, self.limits) {
            Ok(host) => {
                self.host = host;
                self.manifest = manifest;
                self.stamp = Stamp {
                    manifest: manifest_hash,
                    source: source_hash,
                };
                self.faults = 0;
                self.disabled = None;
                self.last_error = None;
                Poll::Reloaded
            }
            Err(e) => {
                self.stamp = Stamp {
                    manifest: manifest_hash,
                    source: source_hash,
                };
                Poll::Failed(format!("扩展 {} 重载失败: {e}", manifest.id))
            }
        }
    }

    /// 文件没变时返回 `Unchanged`,变了才报同一条错,避免每秒刷日志。
    fn stale(&mut self, manifest_hash: u64, error: String) -> Poll {
        let source_hash = self.current_source_hash();
        if manifest_hash == self.stamp.manifest && source_hash == self.stamp.source {
            return Poll::Unchanged;
        }
        self.stamp = Stamp {
            manifest: manifest_hash,
            source: source_hash,
        };
        Poll::Failed(error)
    }

    /// 当前入口脚本的哈希;读不到按 0 算,文件出现后哈希变化会触发重载。
    fn current_source_hash(&self) -> u64 {
        let Some(main) = self.manifest.appearance_main() else {
            return 0;
        };
        std::fs::read(self.dir.join(main))
            .map(|bytes| hash(&bytes))
            .unwrap_or(0)
    }

    fn fault(&mut self, error: String) {
        self.faults = self.faults.saturating_add(1);
        self.last_error = Some(error.clone());
        if self.faults >= MAX_FAULTS && self.disabled.is_none() {
            self.disabled = Some(format!("连续 {MAX_FAULTS} 次失败,已停用: {error}"));
        }
    }
}

/// 目录名必须等于扩展 id。
fn check_dir(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if name != manifest.id {
        return Err(format!(
            "目录名 {name:?} 与扩展 id {:?} 不一致",
            manifest.id
        ));
    }
    Ok(())
}

/// 读文件并限制大小;先看元数据,避免为一个超大文件分配内存。
fn read_capped(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > max as u64 {
        return Err(format!("文件 {} 超过 {max} 字节", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() > max {
        return Err(format!("文件 {} 超过 {max} 字节", path.display()));
    }
    Ok(bytes)
}

/// 读 UTF-8 文本并限制大小。
fn read_text(path: &Path, max: usize) -> Result<String, String> {
    let bytes = read_capped(path, max)?;
    String::from_utf8(bytes).map_err(|e| format!("不是 UTF-8: {e}"))
}

/// FNV-1a 64:只用来判断文件变没变,不用于安全场景。
fn hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
