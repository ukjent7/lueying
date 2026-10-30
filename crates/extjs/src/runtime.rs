//! 多扩展运行期:有序加载、按激活事件懒加载、空闲卸载与 Scope 回收。
//!
//! 每个扩展槽的注册物(命令、触发词、热键)全部来自清单里的 Scope。
//! 卸载槽位时 Scope 与 JS 宿主一起丢掉,宿主侧不需要逐项反注册;热重载
//! 也走同一条路径,旧宿主先整体丢弃再加载新版本。
//!
//! 外观包常驻(它没有激活事件,候选窗每帧都要用);命令包只在命令被调用
//! 时加载,空闲 [`IDLE_UNLOAD`] 后卸载。命令清单来自清单文件,不加载 JS
//! 也能列出来,命令面板与设置页因此没有额外成本。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lueying_extmeta::metrics::{ExtState, ExtStat};
use lueying_extsvc::Registry;

use crate::host::{Invoke, Limits, Settled};
use crate::manifest::{MAX_MANIFEST_BYTES, Manifest, When};
use crate::pack::{Pack, Poll};
use crate::scene::{SceneIn, ScenePatch};
use crate::ConsentGate;

const MANIFEST: &str = "manifest.json";

/// 命令包空闲多久卸载 JS 宿主。
pub const IDLE_UNLOAD: Duration = Duration::from_secs(30);

/// 一条可调用的命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandInfo {
    /// 来源扩展 id。
    pub extension: String,
    /// 完整命令 id:`<扩展 id>.<命令 id>`。
    pub id: String,
    /// 扩展内的短 id。
    pub local: String,
    /// 命令面板与动作候选里显示的文字。
    pub title: String,
    /// 绑定生效的输入状态。
    pub when: When,
}

/// 一个扩展槽声明的注册物快照。卸载后整份消失。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    /// 命令短 id。
    pub commands: Vec<String>,
    /// 输入内触发词。
    pub triggers: Vec<String>,
    /// 建议热键。
    pub hotkeys: Vec<String>,
}

struct Slot {
    id: String,
    dir: PathBuf,
    manifest: Manifest,
    pack: Option<Pack>,
    last_used: Instant,
    error: Option<String>,
    /// 外观包常驻;带激活事件的命令包空闲卸载。
    resident: bool,
}

impl Slot {
    fn scope(&self) -> Scope {
        let activate = self.manifest.activate();
        Scope {
            commands: self
                .manifest
                .commands()
                .iter()
                .map(|command| command.id.clone())
                .collect(),
            triggers: activate
                .map(|activate| activate.triggers.clone())
                .unwrap_or_default(),
            hotkeys: activate
                .map(|activate| activate.hotkeys.clone())
                .unwrap_or_default(),
        }
    }
}

/// 多扩展运行期。
pub struct Runtime {
    limits: Limits,
    /// 服务缝注册表;所有槽位共享,提供者由宿主进程装填。
    services: Arc<Registry>,
    /// 同意检查;装到每个包的加载与热重载路径上。
    gate: Option<ConsentGate>,
    slots: Vec<Slot>,
    idle: Duration,
}

impl Runtime {
    /// 按配置顺序打开扩展目录。
    ///
    /// 坏清单与缺失目录只记一条诊断,不影响其他扩展;返回的诊断由调用方
    /// 写进日志与设置页。重复 id 只保留第一次出现的位置。
    pub fn open(root: &Path, order: &[String], limits: Limits) -> (Self, Vec<String>) {
        Self::open_with(root, order, limits, Arc::new(Registry::new()))
    }

    /// 同上,另带服务注册表;注册表被全部槽位共享。
    pub fn open_with(
        root: &Path,
        order: &[String],
        limits: Limits,
        services: Arc<Registry>,
    ) -> (Self, Vec<String>) {
        Self::open_with_gate(root, order, limits, services, None)
    }

    /// 同上,另带同意检查;热重载前也要先过同一道门。
    pub fn open_with_gate(
        root: &Path,
        order: &[String],
        limits: Limits,
        services: Arc<Registry>,
        gate: Option<ConsentGate>,
    ) -> (Self, Vec<String>) {
        let mut slots = Vec::new();
        let mut errors = Vec::new();
        for id in order {
            if slots.iter().any(|slot: &Slot| slot.id == *id) {
                continue;
            }
            let dir = root.join(id);
            match read_manifest(&dir) {
                Ok(manifest) => {
                    let resident = manifest.activate().is_none();
                    slots.push(Slot {
                        id: id.clone(),
                        dir,
                        manifest,
                        pack: None,
                        last_used: Instant::now(),
                        error: None,
                        resident,
                    });
                }
                Err(error) => errors.push(format!("扩展 {id} 未加载: {error}")),
            }
        }
        (
            Self {
                limits,
                services,
                gate,
                slots,
                idle: IDLE_UNLOAD,
            },
            errors,
        )
    }

    /// 调整空闲卸载时长;测试用零时长验证卸载路径。
    pub fn set_idle(&mut self, idle: Duration) {
        self.idle = idle;
    }

    /// 全部扩展声明的命令;不加载 JS。
    pub fn commands(&self) -> Vec<CommandInfo> {
        let mut out = Vec::new();
        for slot in &self.slots {
            for command in slot.manifest.commands() {
                out.push(CommandInfo {
                    extension: slot.id.clone(),
                    id: full_id(&slot.id, &command.id),
                    local: command.id.clone(),
                    title: command.title.clone(),
                    when: command.when,
                });
            }
        }
        out
    }

    /// 所有槽位的运行指标;未加载的包也有状态与内存上限。
    pub fn stats(&self) -> Vec<ExtStat> {
        self.slots
            .iter()
            .map(|slot| match &slot.pack {
                Some(pack) => pack.stat(),
                None => ExtStat {
                    id: slot.id.clone(),
                    name: slot.manifest.name.clone(),
                    version: slot.manifest.version.clone(),
                    state: if slot.error.is_some() {
                        ExtState::Error
                    } else {
                        ExtState::Idle
                    },
                    memory_limit: self.limits.memory_bytes as u64,
                    last_error: slot.error.clone(),
                    ..ExtStat::default()
                },
            })
            .collect()
    }

    /// 某个扩展声明的注册物;卸载后返回 `None`。
    pub fn scope(&self, id: &str) -> Option<Scope> {
        self.slot(id).map(Slot::scope)
    }

    /// 按完整命令 id 查找来源。
    pub fn command(&self, id: &str) -> Option<CommandInfo> {
        let (extension, local) = id.split_once('.')?;
        let slot = self.slot(extension)?;
        let command = slot
            .manifest
            .commands()
            .iter()
            .find(|command| command.id == local)?;
        Some(CommandInfo {
            extension: extension.to_string(),
            id: id.to_string(),
            local: local.to_string(),
            title: command.title.clone(),
            when: command.when,
        })
    }

    /// 加载一个扩展的 JS 宿主;已经加载就只刷新时间。
    pub fn activate(&mut self, id: &str) -> Result<(), String> {
        let index = self.index(id)?;
        self.ensure(index)
    }

    /// 执行一条完整 id 的命令。
    ///
    /// 命令返回 Promise 时得到 [`Invoke::Pending`],用 [`Runtime::pump`]
    /// 按宿主时钟推进;等待期间扩展照常空闲卸载计时。
    pub fn invoke(&mut self, command: &str, args: &str) -> Result<Invoke, String> {
        let Some((extension, local)) = command.split_once('.') else {
            return Err(format!("命令 id {command:?} 缺少扩展前缀"));
        };
        let index = self.index(extension)?;
        if !self.slots[index]
            .manifest
            .commands()
            .iter()
            .any(|item| item.id == local)
        {
            return Err(format!("扩展 {extension} 没有声明命令 {local}"));
        }
        self.ensure(index)?;
        let slot = &mut self.slots[index];
        let result = slot
            .pack
            .as_mut()
            .expect("ensure 已加载宿主")
            .invoke(local, args);
        slot.last_used = Instant::now();
        if let Err(error) = &result {
            slot.error = Some(error.clone());
        } else {
            slot.error = None;
        }
        result
    }

    /// 把场景派发给已加载的扩展,按配置顺序合并补丁。
    ///
    /// 常驻外观包在第一次派发时加载;命令包不因为候选窗刷新而被拉起。
    pub fn dispatch(&mut self, scene: &SceneIn) -> Option<ScenePatch> {
        if scene.private {
            return None;
        }
        let mut merged: Option<ScenePatch> = None;
        let services = Arc::clone(&self.services);
        let gate = self.gate.clone();
        for slot in self.slots.iter_mut() {
            if slot.pack.is_none() {
                if !slot.resident || slot.manifest.script_main().is_none() {
                    continue;
                }
                match Pack::load_with_gate(&slot.dir, self.limits, &services, gate.clone()) {
                    Ok(pack) => {
                        slot.pack = Some(pack);
                        slot.error = None;
                        slot.last_used = Instant::now();
                    }
                    Err(error) => {
                        slot.error = Some(error);
                        continue;
                    }
                }
            }
            let patch = slot.pack.as_mut().and_then(|pack| pack.dispatch(scene));
            if let Some(patch) = patch {
                merged = Some(match merged {
                    Some(mut current) => {
                        current.merge(patch);
                        current
                    }
                    None => patch,
                });
            }
        }
        merged
    }

    /// 轮询已加载扩展的文件变化;清单被删的槽位直接移除。
    /// 返回移除的槽数,调用方借此清掉残留装饰。
    pub fn poll(&mut self) -> usize {
        let mut removed = Vec::new();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let Some(pack) = slot.pack.as_mut() else {
                continue;
            };
            let mut reloaded = None;
            match pack.poll() {
                Poll::Unchanged => {}
                Poll::Reloaded => reloaded = Some(pack.manifest().clone()),
                Poll::Failed(error) => slot.error = Some(error),
                Poll::Removed => removed.push(index),
            }
            if let Some(manifest) = reloaded {
                slot.manifest = manifest;
                slot.error = None;
            }
        }
        for index in removed.iter().rev().copied() {
            let slot = self.slots.remove(index);
            println!("扩展 {} 已卸载(清单被删)", slot.id);
        }
        removed.len()
    }

    /// 推进全部挂起任务:兑现到点定时器、跑作业队列、收结算结果。
    /// 返回 (扩展 id, 结算) 列表;有活动的扩展刷新空闲计时。
    pub fn pump(&mut self, now_ms: u64) -> Vec<(String, Settled)> {
        let mut out = Vec::new();
        for slot in self.slots.iter_mut() {
            // 不按 pending 跳过:被中断杀掉的任务记账清零后,包里仍可能有
            // 到点的定时器要兑现;这里每次都进,空泵只有微秒级开销。
            let Some(pack) = slot.pack.as_mut() else {
                continue;
            };
            let settled = pack.pump(now_ms);
            if !settled.is_empty() {
                slot.last_used = Instant::now();
            }
            for item in settled {
                out.push((slot.id.clone(), item));
            }
        }
        out
    }

    /// 全部扩展挂起、尚未结算的任务数。
    pub fn pending(&self) -> u64 {
        self.slots
            .iter()
            .filter_map(|slot| slot.pack.as_ref())
            .map(|pack| pack.pending())
            .sum()
    }

    /// 卸载空闲超过阈值的命令包;常驻外观包不动。
    pub fn idle_unload(&mut self) {
        self.idle_unload_at(Instant::now());
    }

    /// 指定时刻的空闲卸载;测试用固定时刻。
    pub fn idle_unload_at(&mut self, now: Instant) {
        for slot in &mut self.slots {
            if slot.resident || slot.pack.is_none() {
                continue;
            }
            if now.saturating_duration_since(slot.last_used) >= self.idle {
                slot.pack = None;
            }
        }
    }

    /// 卸载一个扩展槽:Scope 与 JS 宿主一起消失。
    pub fn unload(&mut self, id: &str) -> bool {
        let Some(index) = self.slots.iter().position(|slot| slot.id == id) else {
            return false;
        };
        self.slots.remove(index);
        true
    }

    /// 丢弃 JS 宿主但保留槽位与命令声明。
    pub fn deactivate(&mut self, id: &str) {
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.id == id) {
            slot.pack = None;
        }
    }

    /// 扩展宿主是否已加载。
    pub fn loaded(&self, id: &str) -> bool {
        self.slot(id).is_some_and(|slot| slot.pack.is_some())
    }

    /// 已加载的扩展数量。
    pub fn loaded_count(&self) -> usize {
        self.slots.iter().filter(|slot| slot.pack.is_some()).count()
    }

    /// 某个扩展最近一次错误。
    pub fn error(&self, id: &str) -> Option<&str> {
        self.slot(id).and_then(|slot| slot.error.as_deref())
    }

    fn slot(&self, id: &str) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.id == id)
    }

    fn index(&self, id: &str) -> Result<usize, String> {
        self.slots
            .iter()
            .position(|slot| slot.id == id)
            .ok_or_else(|| format!("扩展 {id} 未启用"))
    }

    fn ensure(&mut self, index: usize) -> Result<(), String> {
        if self.slots[index].pack.is_some() {
            self.slots[index].last_used = Instant::now();
            return Ok(());
        }
        let limits = self.limits;
        let services = Arc::clone(&self.services);
        let gate = self.gate.clone();
        let slot = &mut self.slots[index];
        match Pack::load_with_gate(&slot.dir, limits, &services, gate) {
            Ok(pack) => {
                slot.pack = Some(pack);
                slot.error = None;
                slot.last_used = Instant::now();
                Ok(())
            }
            Err(error) => {
                slot.error = Some(error.clone());
                Err(error)
            }
        }
    }
}

/// 读清单并检查目录名;不建 JS 运行时。
fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let bytes =
        std::fs::read(dir.join(MANIFEST)).map_err(|error| format!("读清单失败: {error}"))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(format!("清单超过 {MAX_MANIFEST_BYTES} 字节"));
    }
    let manifest = Manifest::parse(&bytes)?;
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
    Ok(manifest)
}

/// 只读一个包目录的清单并核对目录名;设置页与同意检查用,不建 JS 运行时。
pub fn manifest_at(dir: &Path) -> Result<Manifest, String> {
    read_manifest(dir)
}

/// 完整命令 id。
pub fn full_id(extension: &str, command: &str) -> String {
    format!("{extension}.{command}")
}
