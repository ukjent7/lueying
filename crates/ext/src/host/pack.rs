//! 扩展包:清单 + `.wasm` + 热重载。
//!
//! 一个包是一个目录:`manifest.json` 与清单指定的 Wasm 模块。目录名必须
//! 等于清单里的 id。加载失败返回可读原因;热重载保留上一版可用的 worker,
//! 写坏的存盘不会把正在用的扩展弄没。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::manifest::metrics::{ExtStat, ExtState};

use crate::host::ConsentGate;
use crate::host::Finished;
use crate::host::Registry;
use crate::host::worker::Worker;
use crate::host::{MAX_WASM_BYTES, MAX_WASM_BYTES_ENGINE};
use crate::host::{SceneIn, ScenePatch};
use crate::manifest::{Command, MAX_MANIFEST_BYTES, Manifest};

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

/// 文件指纹:清单与 Wasm 模块各一个内容哈希。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    manifest: u64,
    wasm: u64,
}

/// 一个已加载的扩展包。
pub struct Pack {
    dir: PathBuf,
    manifest: Manifest,
    worker: Worker,
    services: Arc<Registry>,
    gate: Option<ConsentGate>,
    stamp: Stamp,
    last_error: Option<String>,
    removed: bool,
    pending: u64,
}

impl Pack {
    /// 加载一个包目录。
    pub fn load(dir: &Path) -> Result<Self, String> {
        Self::load_with_gate(dir, Arc::new(Registry::new()), None)
    }

    /// 用服务注册表与同意检查加载一个包目录。
    ///
    /// 清单 `needs` 只决定同意书里亮出什么,调用期不再门控。
    pub fn load_with_gate(
        dir: &Path,
        services: Arc<Registry>,
        gate: Option<ConsentGate>,
    ) -> Result<Self, String> {
        let manifest_bytes = read_capped(&dir.join(MANIFEST), MAX_MANIFEST_BYTES)
            .map_err(|error| format!("读扩展清单失败: {error}"))?;
        let manifest = Manifest::parse(&manifest_bytes)?;
        check_dir(dir, &manifest)?;
        if let Some(gate) = &gate {
            gate(&manifest, dir)?;
        }
        let wasm_cap = if manifest.engine {
            MAX_WASM_BYTES_ENGINE
        } else {
            MAX_WASM_BYTES
        };
        let wasm = read_capped(&dir.join(manifest.wasm_main()), wasm_cap)
            .map_err(|error| format!("读 Wasm 模块 {} 失败: {error}", manifest.wasm_main()))?;
        let wasm_hash = hash(&wasm);
        let worker = Worker::spawn(wasm, Arc::new(manifest.clone()), Arc::clone(&services))
            .map_err(|error| format!("扩展 {} 加载失败: {error}", manifest.id))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            stamp: Stamp {
                manifest: hash(&manifest_bytes),
                wasm: wasm_hash,
            },
            manifest,
            worker,
            services,
            gate,
            last_error: None,
            removed: false,
            pending: 0,
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

    /// 清单里声明的命令。
    pub fn commands(&self) -> &[Command] {
        self.manifest.commands()
    }

    /// 已提交、尚未结算的命令数。
    pub fn pending(&self) -> u64 {
        self.pending
    }

    /// worker 是否有命令在跑或排队。
    pub fn busy(&self) -> bool {
        self.pending > 0 || self.worker.busy()
    }

    /// 当前运行指标;`top` 与设置页用。
    pub fn stat(&self) -> ExtStat {
        let host = self.worker.metrics();
        ExtStat {
            id: self.manifest.id.clone(),
            name: self.manifest.name.clone(),
            version: self.manifest.version.clone(),
            state: ExtState::Loaded,
            dispatches: host.dispatches,
            dispatch_peak_us: host.dispatch_peak_us,
            pending: self.pending,
            last_error: self.last_error.clone(),
        }
    }

    /// 执行一条已声明的命令;参数与返回值都是 JSON 文本。
    ///
    /// 返回提交序号,结果稍后从 [`Pack::take_results`] 取。命令在扩展自己的
    /// worker 上串行执行,阻塞服务调用只挡住这个 worker。
    pub fn invoke(&mut self, command: &str, args: &str) -> Result<u64, String> {
        self.check_invokable(command)?;
        match self.worker.command(command, args) {
            Ok(seq) => {
                self.pending = self.pending.saturating_add(1);
                Ok(seq)
            }
            Err(error) => {
                self.fault(error.clone());
                Err(error)
            }
        }
    }

    /// 执行一条命令并等它跑完;测试与同步调用方用。
    pub fn invoke_wait(
        &mut self,
        command: &str,
        args: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        self.check_invokable(command)?;
        self.pending = self.pending.saturating_add(1);
        let result = self.worker.command_wait(command, args, timeout);
        self.pending = self.pending.saturating_sub(1);
        match &result {
            Ok(_) => self.succeed(),
            Err(error) => self.fault(error.clone()),
        }
        result
    }

    /// 取走已经完成的命令结果。
    pub fn take_results(&mut self) -> Vec<Finished> {
        let finished = self.worker.drain_results();
        if finished.is_empty() {
            return finished;
        }
        self.pending = self.pending.saturating_sub(finished.len() as u64);
        for item in &finished {
            match &item.result {
                Ok(_) => self.succeed(),
                Err(error) => self.fault(error.clone()),
            }
        }
        finished
    }

    /// 派发一次场景状态。
    ///
    /// 隐私字段直接返回 `None`,guest 不会被调用。worker 忙时跳过这一帧,
    /// 不把长命令算成外观超时。
    pub fn dispatch(&mut self, state: &SceneIn) -> Option<ScenePatch> {
        if state.private || self.removed {
            return None;
        }
        if !self.worker.has_appearance() {
            return None;
        }
        match self.worker.appearance(state) {
            None => None,
            Some(Ok(None)) => {
                self.succeed();
                None
            }
            Some(Ok(Some(patch))) => {
                self.succeed();
                Some(patch)
            }
            Some(Err(error)) => {
                self.fault(error);
                None
            }
        }
    }

    /// 轮询文件变化;每次调用最多读清单与模块各一遍,由调用方控制频率。
    pub fn poll(&mut self) -> Poll {
        if self.removed {
            return Poll::Removed;
        }
        let manifest_path = self.dir.join(MANIFEST);
        let manifest_bytes = match read_capped(&manifest_path, MAX_MANIFEST_BYTES) {
            Ok(bytes) => bytes,
            Err(error) => {
                if !manifest_path.exists() {
                    self.removed = true;
                    return Poll::Removed;
                }
                return self.stale(0, 0, format!("读扩展清单失败: {error}"));
            }
        };
        let manifest_hash = hash(&manifest_bytes);
        let manifest = match Manifest::parse(&manifest_bytes) {
            Ok(manifest) => manifest,
            Err(error) => return self.stale(manifest_hash, 0, error),
        };
        if let Err(error) = check_dir(&self.dir, &manifest) {
            return self.stale(manifest_hash, 0, error);
        }
        let wasm_cap = if manifest.engine {
            MAX_WASM_BYTES_ENGINE
        } else {
            MAX_WASM_BYTES
        };
        let wasm = match read_capped(&self.dir.join(manifest.wasm_main()), wasm_cap) {
            Ok(bytes) => bytes,
            Err(error) => {
                let error = format!("读 Wasm 模块 {} 失败: {error}", manifest.wasm_main());
                return self.stale(manifest_hash, 0, error);
            }
        };
        let stamp = Stamp {
            manifest: manifest_hash,
            wasm: hash(&wasm),
        };
        if stamp == self.stamp {
            return Poll::Unchanged;
        }
        // 先记下新指纹:同意被拒或加载失败时,用户改文件或重新同意才会再试,
        // 不会每秒重试刷日志。
        self.stamp = stamp;
        if let Some(gate) = &self.gate
            && let Err(error) = gate(&manifest, &self.dir)
        {
            let error = format!("扩展 {} 更新未生效: {error}", manifest.id);
            self.last_error = Some(error.clone());
            return Poll::Failed(error);
        }
        let worker =
            match Worker::spawn(wasm, Arc::new(manifest.clone()), Arc::clone(&self.services)) {
                Ok(worker) => worker,
                Err(error) => {
                    let error = format!("扩展 {} 重载失败: {error}", manifest.id);
                    self.last_error = Some(error.clone());
                    return Poll::Failed(error);
                }
            };
        self.worker = worker;
        self.manifest = manifest;
        self.last_error = None;
        self.pending = 0;
        Poll::Reloaded
    }

    fn check_invokable(&self, command: &str) -> Result<(), String> {
        if self.removed {
            return Err(format!("扩展 {} 已卸载", self.manifest.id));
        }
        if !self
            .manifest
            .commands()
            .iter()
            .any(|item| item.id == command)
        {
            return Err(format!("扩展 {} 没有声明命令 {command}", self.manifest.id));
        }
        Ok(())
    }

    fn succeed(&mut self) {
        self.last_error = None;
    }

    fn fault(&mut self, error: String) {
        self.last_error = Some(error);
    }

    /// 文件没变时返回 `Unchanged`,变了才报同一条错,避免每秒刷日志。
    fn stale(&mut self, manifest_hash: u64, wasm_hash: u64, error: String) -> Poll {
        if manifest_hash == self.stamp.manifest && wasm_hash == self.stamp.wasm {
            return Poll::Unchanged;
        }
        self.stamp = Stamp {
            manifest: manifest_hash,
            wasm: wasm_hash,
        };
        self.last_error = Some(error.clone());
        Poll::Failed(error)
    }
}

/// 读清单并核对目录名;不建 Wasm 引擎。
pub(crate) fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let bytes = read_capped(&dir.join(MANIFEST), MAX_MANIFEST_BYTES)
        .map_err(|error| format!("读清单失败: {error}"))?;
    let manifest = Manifest::parse(&bytes)?;
    check_dir(dir, &manifest)?;
    Ok(manifest)
}

/// 目录名必须等于扩展 id。
pub(crate) fn check_dir(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    if manifest.is_native() {
        return Err("原生扩展不经 Wasm 运行时加载".into());
    }
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name != manifest.id {
        return Err(format!(
            "目录名 {name:?} 与扩展 id {:?} 不一致",
            manifest.id
        ));
    }
    Ok(())
}

/// 读文件并限制大小;先看元数据,避免为一个超大文件分配内存。
pub(crate) fn read_capped(path: &Path, max: usize) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if meta.len() > max as u64 {
        return Err(format!("文件 {} 超过 {max} 字节", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    if bytes.len() > max {
        return Err(format!("文件 {} 超过 {max} 字节", path.display()));
    }
    Ok(bytes)
}

/// FNV-1a 64:只用来判断文件变没变,不用于安全场景。
pub(crate) fn hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;
