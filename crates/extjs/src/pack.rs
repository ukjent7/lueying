//! 扩展包:清单 + 入口脚本 + 热重载。
//!
//! 一个包是一个目录:`manifest.json` 与入口脚本(默认 `index.js`)。目录名
//! 必须等于清单里的 id,避免两个包抢同一个配置键。加载失败返回可读原因;
//! 热重载保留上一版可用的宿主,写坏的存盘不会把正在用的扩展弄没。

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use lueying_extmeta::metrics::{ExtStat, ExtState};
use lueying_extsvc::Registry;

use crate::ConsentGate;
use crate::MAX_FAULTS;
use crate::host::{Host, Invoke, Limits, Settled};
use crate::manifest::{Command, MAX_MANIFEST_BYTES, MAX_SOURCE_BYTES, Manifest};
use crate::scene::{SceneIn, ScenePatch};

const MANIFEST: &str = "manifest.json";

/// 没有服务提供者的默认注册表;单包加载(`Pack::load`)用它。
static EMPTY_SERVICES: LazyLock<Arc<Registry>> = LazyLock::new(|| Arc::new(Registry::new()));

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
    /// 服务注册表;热重载时重建宿主要原样带上。
    services: Arc<Registry>,
    /// 同意检查;装了它以后清单/脚本变化都要先获同意才重载。
    gate: Option<ConsentGate>,
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
        Self::load_with_services(dir, limits, &EMPTY_SERVICES)
    }

    /// 用指定上限与服务注册表加载一个包目录。
    ///
    /// 清单 `needs` 喂给引导层做调用面门控:没声明的服务在 JS 侧当场拒绝。
    pub fn load_with_services(
        dir: &Path,
        limits: Limits,
        services: &Arc<Registry>,
    ) -> Result<Self, String> {
        Self::load_with_gate(dir, limits, services, None)
    }

    /// 同上,另带同意检查;加载与热重载都先问它。
    pub fn load_with_gate(
        dir: &Path,
        limits: Limits,
        services: &Arc<Registry>,
        gate: Option<ConsentGate>,
    ) -> Result<Self, String> {
        let manifest_bytes = read_capped(&dir.join(MANIFEST), MAX_MANIFEST_BYTES)
            .map_err(|e| format!("读扩展清单失败: {e}"))?;
        let manifest = Manifest::parse(&manifest_bytes)?;
        check_dir(dir, &manifest)?;
        if let Some(gate) = &gate {
            gate(&manifest, dir)?;
        }
        let main = manifest
            .script_main()
            .ok_or_else(|| format!("扩展 {} 没有脚本入口", manifest.id))?;
        let source = read_text(&dir.join(main), MAX_SOURCE_BYTES)
            .map_err(|e| format!("读入口脚本 {main} 失败: {e}"))?;
        let needs: Vec<&str> = manifest.needs.iter().map(|service| service.key()).collect();
        let host =
            Host::new_with_caller(&source, limits, &needs, Arc::clone(services), &manifest.id)
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
            services: Arc::clone(services),
            gate,
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

    /// 最近一次失败的原因;成功后清空。
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// 连续失败被停用的原因;重载成功后清空。
    pub fn disabled_reason(&self) -> Option<&str> {
        self.disabled.as_deref()
    }

    /// 清单里声明的命令。
    pub fn commands(&self) -> &[Command] {
        self.manifest.commands()
    }

    /// 当前运行指标;`top` 与设置页用。
    pub fn stat(&self) -> ExtStat {
        let host = self.host.metrics();
        ExtStat {
            id: self.manifest.id.clone(),
            name: self.manifest.name.clone(),
            version: self.manifest.version.clone(),
            state: if self.disabled.is_some() {
                ExtState::Disabled
            } else {
                ExtState::Loaded
            },
            memory_used: host.memory_used,
            memory_limit: self.limits.memory_bytes as u64,
            dispatches: host.dispatches,
            steps: host.steps,
            dispatch_peak_us: host.dispatch_peak_us,
            step_peak_us: host.step_peak_us,
            timeouts: host.timeouts,
            ooms: host.ooms,
            faults: self.faults,
            pending: self.host.pending(),
            last_error: self.disabled.clone().or_else(|| self.last_error.clone()),
        }
    }

    /// 执行一条已声明的命令;参数与返回值都是 JSON 文本。
    ///
    /// 未声明的命令直接拒绝,JS 侧即使注册了同名函数也调不到;命令失败
    /// 与派发失败共用连续失败计数。命令返回 Promise 时得到
    /// [`Invoke::Pending`],由 [`Pack::pump`] 推进。
    pub fn invoke(&mut self, command: &str, args: &str) -> Result<Invoke, String> {
        if self.removed {
            return Err(format!("扩展 {} 已卸载", self.manifest.id));
        }
        if let Some(reason) = &self.disabled {
            return Err(format!("扩展 {} 已停用: {reason}", self.manifest.id));
        }
        if !self.manifest.commands().iter().any(|c| c.id == command) {
            return Err(format!("扩展 {} 没有声明命令 {command}", self.manifest.id));
        }
        match self.host.invoke(command, args) {
            Ok(invoke) => {
                self.faults = 0;
                self.last_error = None;
                Ok(invoke)
            }
            Err(error) => {
                self.fault(error.clone());
                Err(error)
            }
        }
    }

    /// 推进挂起任务:兑现到点定时器、跑一轮作业队列、收结算结果。
    ///
    /// 结算失败计入连续失败计数;停用或卸载后挂起任务不再推进。
    pub fn pump(&mut self, now_ms: u64) -> Vec<Settled> {
        if self.removed || self.disabled.is_some() {
            return Vec::new();
        }
        let before = self.host.violations();
        let settled = self.host.pump(now_ms);
        // 作业被步进预算打断也算一次失败;反复超限与命令失败同样会停用。
        if self.host.violations() > before {
            self.fault(format!("扩展 {} 作业步超出预算", self.manifest.id));
        }
        for item in &settled {
            if item.ok {
                self.faults = 0;
                self.last_error = None;
            } else {
                self.fault(item.error.clone().unwrap_or_default());
            }
        }
        settled
    }

    /// 挂起、尚未结算的任务数。
    pub fn pending(&self) -> u64 {
        self.host.pending()
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
        let Some(main) = manifest.script_main().map(str::to_string) else {
            return self.stale(manifest_hash, format!("扩展 {} 没有脚本入口", manifest.id));
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
        // 新内容先过同意检查;不同意就不换宿主,旧版本继续跑。stamp 已经
        // 记到新内容,用户同意后由宿主重建 Runtime,不会每秒重试刷日志。
        let denied = self
            .gate
            .as_ref()
            .and_then(|gate| gate(&manifest, &self.dir).err());
        if let Some(error) = denied {
            self.stamp = Stamp {
                manifest: manifest_hash,
                source: source_hash,
            };
            let error = format!("扩展 {} 更新未生效: {error}", manifest.id);
            self.last_error = Some(error.clone());
            return Poll::Failed(error);
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
        let needs: Vec<&str> = manifest.needs.iter().map(|service| service.key()).collect();
        match Host::new_with_caller(
            &source,
            self.limits,
            &needs,
            Arc::clone(&self.services),
            &manifest.id,
        ) {
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
        let Some(main) = self.manifest.script_main() else {
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
