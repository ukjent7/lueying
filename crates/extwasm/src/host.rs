//! wasmi 宿主:一个扩展一个 worker,线程内持有 `Store`、实例与服务导入。
//!
//! 命令与外观都在 worker 上串行执行;宿主服务调用是阻塞的,但只阻塞这个
//! worker,不碰引擎线程。trap 后实例不可再进入,下一次调用前重建。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use lueying_extproto::{MAX_RESULT_BYTES, SceneIn, ScenePatch};
use lueying_extsvc::Registry;
use wasmi::{
    AsContext, Caller, Config, Engine, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, TypedFunc,
};

use crate::Manifest;
use crate::manifest::Service;

/// worker 线程的宿主栈。Wasm 值栈另有 [`Limits::stack_bytes`] 约束;
/// 这里要容纳 serde 解析与宿主调用,固定 1 MiB。
const WORKER_STACK: usize = 1 << 20;

/// 运行期上限。
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// 线性内存上限(字节)。
    pub memory_bytes: usize,
    /// Wasm 值栈上限(字节)。
    pub stack_bytes: usize,
    /// 初始化时限(毫秒);指标展示用。
    pub init_ms: u64,
    /// 单次调用时限(毫秒);指标展示用。
    pub dispatch_ms: u64,
    /// 初始化燃料。
    pub init_fuel: u64,
    /// 命令燃料;纯计算烧完即 trap。
    pub command_fuel: u64,
    /// 外观燃料;候选窗每帧都要跑,预算更紧。
    pub appearance_fuel: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 16 << 20,
            stack_bytes: 256 << 10,
            init_ms: 100,
            dispatch_ms: 100,
            init_fuel: 50_000_000,
            command_fuel: 200_000_000,
            appearance_fuel: 20_000_000,
        }
    }
}

/// 一个 worker 的运行指标快照。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostMetrics {
    /// 线性内存当前占用(字节)。
    pub memory_used: u64,
    /// 命令/外观调用次数。
    pub dispatches: u64,
    /// 燃料消耗累计。
    pub steps: u64,
    /// 单次调用最慢耗时(微秒)。
    pub dispatch_peak_us: u64,
    /// 分段峰值(微秒);当前与派发峰值相同。
    pub step_peak_us: u64,
    /// 燃料或宿主调用超限次数。
    pub timeouts: u64,
    /// 内存增长失败次数。
    pub ooms: u64,
}

#[derive(Default)]
struct Metrics {
    memory_used: AtomicU64,
    dispatches: AtomicU64,
    steps: AtomicU64,
    dispatch_peak_us: AtomicU64,
    step_peak_us: AtomicU64,
    timeouts: AtomicU64,
    ooms: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> HostMetrics {
        HostMetrics {
            memory_used: self.memory_used.load(Ordering::Relaxed),
            dispatches: self.dispatches.load(Ordering::Relaxed),
            steps: self.steps.load(Ordering::Relaxed),
            dispatch_peak_us: self.dispatch_peak_us.load(Ordering::Relaxed),
            step_peak_us: self.step_peak_us.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
            ooms: self.ooms.load(Ordering::Relaxed),
        }
    }
}

/// guest 通过 `lueying_emit` 交回的最后一段结果。
struct Emitted {
    bytes: Vec<u8>,
    ok: bool,
}

/// `Store` 的宿主状态。
struct HostState {
    caller: String,
    services: Arc<Registry>,
    cancelled: Arc<AtomicBool>,
    limits: StoreLimits,
    memory: Option<Memory>,
    result_ptr: u32,
    result_cap: u32,
    emitted: Option<Emitted>,
    metrics: Arc<Metrics>,
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
    limits: Limits,
    seq: AtomicU64,
}

impl Worker {
    /// 编译好的模块起一个 worker;初始化失败时返回错误,线程已回收。
    pub(crate) fn spawn(
        engine: Arc<Engine>,
        module: Arc<Module>,
        manifest: Arc<Manifest>,
        services: Arc<Registry>,
        limits: Limits,
    ) -> Result<Self, String> {
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
                let mut guest = match Guest::new(
                    engine,
                    module,
                    manifest,
                    services,
                    limits,
                    Arc::clone(&worker_cancelled),
                    Arc::clone(&worker_metrics),
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
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self {
                jobs: jobs_tx,
                results,
                handle: Some(handle),
                cancelled,
                metrics,
                busy,
                has_appearance,
                limits,
                seq: AtomicU64::new(0),
            }),
            Ok(Err(error)) => {
                let _ = handle.join();
                Err(error)
            }
            Err(_) => {
                cancelled.store(true, Ordering::Relaxed);
                let _ = jobs_tx.send(Job::Shutdown);
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
        let wait = Duration::from_millis(self.limits.dispatch_ms.max(50) + 50);
        Some(
            answer
                .recv_timeout(wait)
                .unwrap_or_else(|_| Err("外观派发超时".into())),
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

/// 一个已实例化的 guest;trap 后由下一次调用重建。
struct Guest {
    engine: Arc<Engine>,
    module: Arc<Module>,
    manifest: Arc<Manifest>,
    services: Arc<Registry>,
    limits: Limits,
    cancelled: Arc<AtomicBool>,
    metrics: Arc<Metrics>,
    store: Store<HostState>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    free: TypedFunc<(i32, i32), ()>,
    init: Option<TypedFunc<(), i32>>,
    command: Option<TypedFunc<(i32, i32, i32, i32), i32>>,
    appearance: Option<TypedFunc<(i32, i32), i32>>,
    needs_reset: bool,
}

impl Guest {
    fn new(
        engine: Arc<Engine>,
        module: Arc<Module>,
        manifest: Arc<Manifest>,
        services: Arc<Registry>,
        limits: Limits,
        cancelled: Arc<AtomicBool>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, String> {
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.memory_bytes)
            .table_elements(10_000)
            .instances(1)
            .tables(1)
            .memories(1)
            // 超限就 trap:返回 -1 会让死循环 guest 一直烧燃料,错误归因会漂移。
            .trap_on_grow_failure(true)
            .build();
        let state = HostState {
            caller: manifest.id.clone(),
            services: Arc::clone(&services),
            cancelled: Arc::clone(&cancelled),
            limits: store_limits,
            memory: None,
            result_ptr: 0,
            result_cap: 0,
            emitted: None,
            metrics: Arc::clone(&metrics),
        };
        let mut store = Store::new(&engine, state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(limits.init_fuel)
            .map_err(|error| format!("燃料初始化失败: {error}"))?;
        let mut linker = Linker::new(&engine);
        linker
            .func_wrap("lueying", "emit", host_emit)
            .map_err(|error| format!("注册 emit 失败: {error}"))?;
        linker
            .func_wrap("lueying", "timeout", host_timeout)
            .map_err(|error| format!("注册 timeout 失败: {error}"))?;
        for service in &manifest.needs {
            linker
                .func_wrap("lueying", service.key(), service_import(*service))
                .map_err(|error| format!("注册服务 {} 失败: {error}", service.key()))?;
        }
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|error| format!("扩展 {} 实例化失败: {error}", manifest.id))?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| format!("扩展 {} 没有导出 memory", manifest.id))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&store, "lueying_alloc")
            .map_err(|error| format!("扩展 {} 没有导出 lueying_alloc: {error}", manifest.id))?;
        let free = instance
            .get_typed_func::<(i32, i32), ()>(&store, "lueying_free")
            .map_err(|error| format!("扩展 {} 没有导出 lueying_free: {error}", manifest.id))?;
        let result_ptr = instance
            .get_typed_func::<(), i32>(&store, "lueying_result_ptr")
            .and_then(|func| func.call(&mut store, ()))
            .map_err(|error| format!("扩展 {} 没有可用的结果缓冲区: {error}", manifest.id))?;
        let result_cap = instance
            .get_typed_func::<(), i32>(&store, "lueying_result_cap")
            .and_then(|func| func.call(&mut store, ()))
            .map_err(|error| format!("扩展 {} 没有可用的结果容量: {error}", manifest.id))?;
        if result_ptr <= 0 || result_cap <= 0 {
            return Err(format!("扩展 {} 的结果缓冲区为空", manifest.id));
        }
        let result_cap = (result_cap as usize).min(MAX_RESULT_BYTES);
        store.data_mut().memory = Some(memory.clone());
        store.data_mut().result_ptr = result_ptr as u32;
        store.data_mut().result_cap = result_cap as u32;
        let init = instance
            .get_typed_func::<(), i32>(&store, "lueying_init")
            .ok();
        let command = instance
            .get_typed_func::<(i32, i32, i32, i32), i32>(&store, "lueying_command")
            .ok();
        let appearance = instance
            .get_typed_func::<(i32, i32), i32>(&store, "lueying_appearance")
            .ok();
        if manifest.is_appearance() && appearance.is_none() {
            return Err(format!(
                "扩展 {} 声明了 appearance,但没有导出 lueying_appearance",
                manifest.id
            ));
        }
        if !manifest.commands().is_empty() && command.is_none() {
            return Err(format!(
                "扩展 {} 声明了命令,但没有导出 lueying_command",
                manifest.id
            ));
        }
        if appearance.is_none() && command.is_none() {
            return Err(format!("扩展 {} 没有可调用的入口", manifest.id));
        }
        if let Some(init) = &init {
            store
                .set_fuel(limits.init_fuel)
                .map_err(|error| format!("燃料初始化失败: {error}"))?;
            let status = init
                .call(&mut store, ())
                .map_err(|error| format!("扩展 {} 初始化失败: {error}", manifest.id))?;
            if status != 0 {
                let text = store
                    .data_mut()
                    .emitted
                    .take()
                    .map(|emitted| String::from_utf8_lossy(&emitted.bytes).into_owned())
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| "没有错误文本".into());
                return Err(format!("扩展 {} 初始化失败: {text}", manifest.id));
            }
        }
        metrics
            .memory_used
            .store(memory.data_size(&store) as u64, Ordering::Relaxed);
        Ok(Self {
            engine,
            module,
            manifest,
            services,
            limits,
            cancelled,
            metrics,
            store,
            memory,
            alloc,
            free,
            init,
            command,
            appearance,
            needs_reset: false,
        })
    }

    fn rebuild(&mut self) -> Result<(), String> {
        let rebuilt = Self::new(
            Arc::clone(&self.engine),
            Arc::clone(&self.module),
            Arc::clone(&self.manifest),
            Arc::clone(&self.services),
            self.limits,
            Arc::clone(&self.cancelled),
            Arc::clone(&self.metrics),
        )?;
        *self = rebuilt;
        Ok(())
    }

    fn command(&mut self, id: &str, args: &str) -> Result<String, String> {
        if self.needs_reset {
            self.rebuild()?;
        }
        let Some(command) = self.command.clone() else {
            return Err("模块没有导出 lueying_command".into());
        };
        self.store
            .set_fuel(self.limits.command_fuel)
            .map_err(|error| format!("燃料设置失败: {error}"))?;
        let started = Instant::now();
        let id_ptr = self.write(id.as_bytes())?;
        let args_ptr = self.write(args.as_bytes())?;
        self.store.data_mut().emitted = None;
        let outcome = command.call(
            &mut self.store,
            (
                id_ptr,
                i32::try_from(id.len()).unwrap_or(i32::MAX),
                args_ptr,
                i32::try_from(args.len()).unwrap_or(i32::MAX),
            ),
        );
        self.free(id_ptr, id.len());
        self.free(args_ptr, args.len());
        self.note(started, self.limits.command_fuel);
        match outcome {
            Ok(0) => self.take_emitted(),
            Ok(_) => Err(self
                .take_emitted()
                .err()
                .unwrap_or_else(|| format!("命令 {id} 返回失败但没有错误文本"))),
            Err(error) => {
                self.needs_reset = true;
                self.record_trap(&error);
                Err(format!("命令 {id} 执行失败: {error}"))
            }
        }
    }

    fn appearance(&mut self, scene: &SceneIn) -> Result<Option<ScenePatch>, String> {
        if self.needs_reset {
            self.rebuild()?;
        }
        let Some(appearance) = self.appearance.clone() else {
            return Err("模块没有导出 lueying_appearance".into());
        };
        let json = scene.to_json()?;
        self.store
            .set_fuel(self.limits.appearance_fuel)
            .map_err(|error| format!("燃料设置失败: {error}"))?;
        let started = Instant::now();
        let ptr = self.write(json.as_bytes())?;
        self.store.data_mut().emitted = None;
        let outcome = appearance.call(
            &mut self.store,
            (ptr, i32::try_from(json.len()).unwrap_or(i32::MAX)),
        );
        self.free(ptr, json.len());
        self.note(started, self.limits.appearance_fuel);
        match outcome {
            Ok(0) => {
                let text = self.take_emitted()?;
                if text.is_empty() {
                    Ok(None)
                } else {
                    ScenePatch::parse(&text).map(Some)
                }
            }
            Ok(_) => Err(self
                .take_emitted()
                .err()
                .unwrap_or_else(|| "外观派发失败但没有错误文本".into())),
            Err(error) => {
                self.needs_reset = true;
                self.record_trap(&error);
                Err(format!("外观派发失败: {error}"))
            }
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<i32, String> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let len = i32::try_from(bytes.len()).map_err(|_| "输入超过 i32 长度".to_string())?;
        let ptr = self
            .alloc
            .call(&mut self.store, len)
            .map_err(|error| format!("guest 分配输入失败: {error}"))?;
        if ptr <= 0 {
            return Err("guest 分配输入失败".into());
        }
        self.memory
            .write(&mut self.store, ptr as usize, bytes)
            .map_err(|error| format!("写 guest 内存失败: {error}"))?;
        Ok(ptr)
    }

    fn free(&mut self, ptr: i32, len: usize) {
        if ptr <= 0 {
            return;
        }
        let len = i32::try_from(len).unwrap_or(i32::MAX);
        let _ = self.free.call(&mut self.store, (ptr, len));
    }

    fn take_emitted(&mut self) -> Result<String, String> {
        let Some(emitted) = self.store.data_mut().emitted.take() else {
            return Ok(String::new());
        };
        let text =
            String::from_utf8(emitted.bytes).map_err(|_| "guest 结果不是 UTF-8".to_string())?;
        if emitted.ok { Ok(text) } else { Err(text) }
    }

    fn note(&mut self, started: Instant, budget: u64) {
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let used = self
            .store
            .get_fuel()
            .map(|left| budget.saturating_sub(left))
            .unwrap_or(0);
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);
        self.metrics.steps.fetch_add(used, Ordering::Relaxed);
        self.metrics
            .dispatch_peak_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
        self.metrics
            .step_peak_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
        self.metrics
            .memory_used
            .store(self.memory.data_size(&self.store) as u64, Ordering::Relaxed);
    }

    fn record_trap(&mut self, error: &wasmi::Error) {
        let text = error.to_string().to_ascii_lowercase();
        if text.contains("fuel") {
            self.metrics.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        // 内存限额触发时 wasmi 报 "growth operation limited" 或内存越界。
        if text.contains("memory") || text.contains("growth") {
            self.metrics.ooms.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 宿主侧 `lueying_emit(ptr, len, ok)`:把 guest 的最终结果拷到宿主。
fn host_emit(caller: Caller<'_, HostState>, ptr: i32, len: i32, ok: i32) {
    let Some(memory) = caller.data().memory.clone() else {
        return;
    };
    let Ok(len) = usize::try_from(len) else {
        return;
    };
    if len > MAX_RESULT_BYTES {
        return;
    }
    let Ok(offset) = usize::try_from(ptr) else {
        return;
    };
    let mut bytes = vec![0; len];
    let read = memory.read(&mut caller, offset, &mut bytes);
    if read.is_ok() {
        caller.data_mut().emitted = Some(Emitted { bytes, ok: ok != 0 });
    }
}

/// 宿主侧 `lueying_timeout(ms)`:可取消的睡眠,卸载时 50 ms 内返回。
fn host_timeout(caller: Caller<'_, HostState>, ms: i64) -> i32 {
    let cancelled = Arc::clone(&caller.data().cancelled);
    let mut left = ms.clamp(0, 600_000) as u64;
    while left > 0 {
        if cancelled.load(Ordering::Relaxed) {
            return -1;
        }
        let step = left.min(50);
        std::thread::sleep(Duration::from_millis(step));
        left -= step;
    }
    if cancelled.load(Ordering::Relaxed) {
        -1
    } else {
        0
    }
}

/// 一个服务的导入实现;方法名与参数都从 guest 内存读。
fn service_import(
    service: Service,
) -> impl Fn(Caller<'_, HostState>, i32, i32, i32, i32) -> i32 + Send + Sync + 'static {
    move |caller, method_ptr, method_len, args_ptr, args_len| {
        let method = match read_string(&caller, method_ptr, method_len) {
            Ok(text) => text,
            Err(error) => return write_result(caller, &error_bytes(&error)),
        };
        let args = match read_string(&caller, args_ptr, args_len) {
            Ok(text) => text,
            Err(error) => return write_result(caller, &error_bytes(&error)),
        };
        let caller_id = caller.data().caller.clone();
        let services = Arc::clone(&caller.data().services);
        let cancelled = Arc::clone(&caller.data().cancelled);
        let timeout = timeout_for(service, &method);
        let result = services.call_cancellable(
            &caller_id,
            service.key(),
            &method,
            &args,
            timeout,
            Some(&cancelled),
        );
        let envelope = match result {
            Ok(value) => {
                let value = serde_json::from_str::<serde_json::Value>(&value)
                    .unwrap_or(serde_json::Value::String(value));
                serde_json::json!({ "ok": true, "value": value })
            }
            Err(error) => error_envelope(&error),
        };
        let bytes = serde_json::to_vec(&envelope).unwrap_or_default();
        write_result(caller, &bytes)
    }
}

fn error_envelope(error: &str) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": error })
}

fn error_bytes(error: &str) -> Vec<u8> {
    serde_json::to_vec(&error_envelope(error)).unwrap_or_default()
}

/// 从 guest 线性内存读一段 UTF-8 文本。
fn read_string(caller: &Caller<'_, HostState>, ptr: i32, len: i32) -> Result<String, String> {
    let memory = caller.data().memory.clone().ok_or("实例没有内存")?;
    let len = usize::try_from(len).map_err(|_| "文本长度为负".to_string())?;
    if len == 0 {
        return Ok(String::new());
    }
    if len > MAX_RESULT_BYTES {
        return Err(format!("文本超过 {MAX_RESULT_BYTES} 字节"));
    }
    let offset = usize::try_from(ptr).map_err(|_| "文本指针为负".to_string())?;
    let mut bytes = vec![0; len];
    memory
        .read(caller.as_context(), offset, &mut bytes)
        .map_err(|error| format!("读 guest 内存失败: {error}"))?;
    String::from_utf8(bytes).map_err(|_| "guest 文本不是 UTF-8".to_string())
}

/// 把结果信封写进 guest 的结果缓冲区,返回写入长度。
fn write_result(caller: Caller<'_, HostState>, bytes: &[u8]) -> i32 {
    let Some(memory) = caller.data().memory.clone() else {
        return -1;
    };
    let ptr = caller.data().result_ptr as usize;
    let cap = caller.data().result_cap as usize;
    let payload = if bytes.len() <= cap {
        bytes.to_vec()
    } else {
        error_bytes(&format!("服务结果超过 {cap} 字节"))
    };
    if payload.len() > cap {
        return -1;
    }
    match memory.write(caller, ptr, &payload) {
        Ok(()) => i32::try_from(payload.len()).unwrap_or(-1),
        Err(_) => -1,
    }
}

/// 服务的宿主等待时限。面板要等用户操作,单独放宽。
fn timeout_for(service: Service, method: &str) -> Duration {
    match (service, method) {
        (Service::Panel, "show") => Duration::from_secs(600),
        (Service::Ocr, "recognize") => Duration::from_secs(120),
        (Service::Capture, "shot") => Duration::from_secs(30),
        _ => Duration::from_secs(10),
    }
}

/// 创建一个 wasmi 引擎;燃料与资源上限按 [`Limits`] 配。
pub(crate) fn engine(limits: Limits) -> Result<Engine, String> {
    let mut config = Config::default();
    config.consume_fuel(true);
    config.allow_start_fn(false);
    config.set_max_stack_height(limits.stack_bytes.max(64 << 10));
    config.set_max_recursion_depth(128);
    Ok(Engine::new(&config))
}

/// 编译一个模块;二进制与 WAT 文本都收。
pub(crate) fn compile(engine: &Engine, bytes: &[u8]) -> Result<Module, String> {
    Module::new(engine, bytes).map_err(|error| format!("Wasm 模块编译失败: {error}"))
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
