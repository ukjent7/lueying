//! 运行期扩展宿主:每个扩展一个 worker,线程内跑 Wasm 组件。
//!
//! 扩展是标准 WebAssembly 组件(WASI 0.3 组件模型):宿主按 `lueying.wit`
//! 契约类型化调用 `init`/`command`/`appearance`,宿主服务以 WIT 导入提供。
//!
//! 命令与外观都在 worker 上串行执行;宿主服务是阻塞调用:宿主把调用交给
//! 服务缝的提供者,等它经 [`Done`] 回传,期间不碰引擎线程。
//!
//! # ABI
//!
//! - 命令:`entry.command(id, args)` 收参数 JSON,回结果 JSON。
//! - 外观:`entry.appearance(scene)` 收场景 JSON,回补丁 JSON 或空。
//! - 服务:WIT 导入接口按包路径镜像;服务接口全量链接,放行按调用逐次
//!   核清单 `needs`,没声明的服务一调即拒。
//!
//! # 边界
//!
//! - 不设燃料、内存或时间预算:扩展跑多久、占多少内存由扩展自己决定。
//! - 隐私字段([`SceneIn::private`])直接跳过,guest 不会收到那次状态。
//! - 清单没有任何命令且不提供外观的包,加载期就拒绝。
//!
//! 打包与作者侧 SDK 见 `crates/extguest`;完整记录见 `docs/extensions/wasm.md`。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use lueying_extproto::{SceneIn, ScenePatch};

use crate::Manifest;
use crate::Registry;
use crate::manifest::Service;

/// worker 线程栈:wasmtime 的 wasm 值栈(默认 512 KiB)与宿主函数的 serde
/// 解析都装在它里面。
const WORKER_STACK: usize = 2 << 20;

/// 一个 worker 的运行指标快照。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostMetrics {
    /// 命令/外观调用次数。
    pub dispatches: u64,
    /// 单次调用最慢耗时(微秒)。
    pub dispatch_peak_us: u64,
}

#[derive(Default)]
pub(crate) struct Metrics {
    pub(crate) dispatches: AtomicU64,
    pub(crate) dispatch_peak_us: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> HostMetrics {
        HostMetrics {
            dispatches: self.dispatches.load(Ordering::Relaxed),
            dispatch_peak_us: self.dispatch_peak_us.load(Ordering::Relaxed),
        }
    }
}

/// 一条命令的最终结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    /// 提交时分配的序号;同一扩展内递增。
    pub seq: u64,
    /// 命令短 id。
    pub command: String,
    /// JSON 结果或错误文本。
    pub result: Result<String, String>,
}

/// worker 的任务。命令结果走独立通道,外观结果按请求回传。
enum Job {
    Command {
        seq: u64,
        id: String,
        args: String,
        reply: Option<mpsc::Sender<Result<String, String>>>,
    },
    Appearance {
        scene: SceneIn,
        reply: mpsc::Sender<Result<Option<ScenePatch>, String>>,
    },
    Shutdown,
}

/// 一个扩展的 worker 句柄。
pub struct Worker {
    jobs: mpsc::Sender<Job>,
    results: mpsc::Receiver<Finished>,
    handle: Option<JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
    metrics: Arc<Metrics>,
    busy: Arc<AtomicBool>,
    has_appearance: bool,
    seq: AtomicU64,
}

impl Worker {
    /// 一个扩展的模块起一个 worker;加载失败时返回错误,线程已回收。
    ///
    /// 格式与入口在编译前检查:都是常数时间的判断,坏模块报模块的错,
    /// 空入口不必等大组件编完才发现。引擎与组件在调用线程上编译,编译大
    /// 组件(JS 引擎、CPython 之类)要几分钟,不占 worker 线程。
    pub(crate) fn spawn(
        wasm: Vec<u8>,
        manifest: Arc<Manifest>,
        services: Arc<Registry>,
    ) -> Result<Self, String> {
        if !crate::component::is_component(&wasm) {
            return Err("不是 WebAssembly 组件:核心模块要用 wasm-tools component new 打包".into());
        }
        if manifest.commands().is_empty() && !manifest.is_appearance() {
            return Err(format!("扩展 {} 没有可调用的入口", manifest.id));
        }
        let engine = crate::component::build_engine()?;
        let component = crate::component::build_component(&engine, &wasm)?;
        let (jobs_tx, jobs_rx) = mpsc::channel();
        let (results_tx, results) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let metrics = Arc::new(Metrics::default());
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_busy = Arc::clone(&busy);
        let worker_metrics = Arc::clone(&metrics);
        let caller = manifest.id.clone();
        let has_appearance = manifest.is_appearance();
        let handle = std::thread::Builder::new()
            .name(format!("ext-wasm-{caller}"))
            .stack_size(WORKER_STACK)
            .spawn(move || {
                let mut guest = match crate::component::ComponentGuest::new(
                    engine,
                    component,
                    manifest,
                    services,
                    Arc::clone(&worker_cancelled),
                    worker_metrics,
                ) {
                    Ok(guest) => {
                        let _ = ready_tx.send(Ok(()));
                        guest
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                loop {
                    match jobs_rx.recv() {
                        Ok(Job::Command {
                            seq,
                            id,
                            args,
                            reply,
                        }) => {
                            let result = guest.command(&id, &args);
                            worker_busy.store(false, Ordering::Relaxed);
                            match reply {
                                Some(reply) => {
                                    let _ = reply.send(result);
                                }
                                None => {
                                    let _ = results_tx.send(Finished {
                                        seq,
                                        command: id,
                                        result,
                                    });
                                }
                            }
                        }
                        Ok(Job::Appearance { scene, reply }) => {
                            let result = guest.appearance(&scene);
                            let _ = reply.send(result);
                        }
                        Ok(Job::Shutdown) | Err(_) => break,
                    }
                }
            })
            .map_err(|error| format!("扩展 {caller} worker 启动失败: {error}"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                jobs: jobs_tx,
                results,
                handle: Some(handle),
                cancelled,
                metrics,
                busy,
                has_appearance,
                seq: AtomicU64::new(0),
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            // 线程在就绪前退出只可能是 panic:发送端随之丢弃。
            Err(_) => {
                let _ = handle.join();
                Err(format!("扩展 {caller} worker 没有就绪"))
            }
        }
    }

    /// 提交一条命令;结果稍后从 [`Worker::drain_results`] 取。
    pub(crate) fn command(&self, id: &str, args: &str) -> Result<u64, String> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        // 提交即标记忙:排队中的命令也要让外观派发跳过,避免白等一次超时。
        self.busy.store(true, Ordering::Relaxed);
        if let Err(error) = self.jobs.send(Job::Command {
            seq,
            id: id.to_string(),
            args: args.to_string(),
            reply: None,
        }) {
            self.busy.store(false, Ordering::Relaxed);
            return Err(format!("扩展 worker 已退出: {error}"));
        }
        Ok(seq)
    }

    /// 提交一条命令并等它跑完;测试与同步调用方用。
    pub(crate) fn command_wait(
        &self,
        id: &str,
        args: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let (reply, answer) = mpsc::channel();
        self.busy.store(true, Ordering::Relaxed);
        if let Err(error) = self.jobs.send(Job::Command {
            seq: 0,
            id: id.to_string(),
            args: args.to_string(),
            reply: Some(reply),
        }) {
            self.busy.store(false, Ordering::Relaxed);
            return Err(format!("扩展 worker 已退出: {error}"));
        }
        answer
            .recv_timeout(timeout)
            .map_err(|_| format!("命令 {id} 等待超时"))?
    }

    /// 派发一次外观;worker 忙时返回 `None`,不把长命令算成外观超时。
    pub(crate) fn appearance(&self, scene: &SceneIn) -> Option<Result<Option<ScenePatch>, String>> {
        if self.busy.load(Ordering::Relaxed) {
            return None;
        }
        let (reply, answer) = mpsc::channel();
        if self
            .jobs
            .send(Job::Appearance {
                scene: scene.clone(),
                reply,
            })
            .is_err()
        {
            return Some(Err("扩展 worker 已退出".into()));
        }
        Some(
            answer
                .recv()
                .unwrap_or_else(|_| Err("扩展 worker 已退出".into())),
        )
    }

    /// 取走已经完成的命令结果。
    pub(crate) fn drain_results(&self) -> Vec<Finished> {
        let mut out = Vec::new();
        while let Ok(result) = self.results.try_recv() {
            out.push(result);
        }
        out
    }

    /// 是否有命令正在执行。
    pub(crate) fn busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed)
    }

    /// 是否声明并导出了外观。
    pub(crate) fn has_appearance(&self) -> bool {
        self.has_appearance
    }

    /// 当前指标快照。
    pub(crate) fn metrics(&self) -> HostMetrics {
        self.metrics.snapshot()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        let _ = self.jobs.send(Job::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// 服务的宿主等待时限。面板要等用户操作,识别要跑满传输时限,单独放宽。
pub(crate) fn timeout_for(service: Service, method: &str) -> Duration {
    match (service, method) {
        (Service::Panel, "show") => Duration::from_secs(600),
        (Service::Ocr, "recognize") => Duration::from_secs(120),
        (Service::Capture, "shot") => Duration::from_secs(30),
        (Service::Network, "fetch") => Duration::from_secs(140),
        (Service::Audio, "record") => Duration::from_secs(40),
        (Service::Asr, "recognize") => Duration::from_secs(120),
        _ => Duration::from_secs(10),
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
