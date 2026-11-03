//! Extism 运行期扩展宿主:每个扩展一个 worker,线程内跑 Extism 插件。
//!
//! 扩展就是标准 Extism 插件:wasm 模块从 `extism:host/env` 导入内核函数
//! (分配、输入输出、错误),需要宿主服务时再从 `extism:host/user` 导入
//! `lueying_<服务>`。宿主用 extism 的运行期执行插件,别的 Extism 宿主写的
//! 插件也能直接放进扩展目录。
//!
//! 命令与外观都在 worker 上串行执行;宿主服务是阻塞调用:宿主把调用交给
//! [`lueying_extsvc`] 的提供者,等它经 [`Done`] 回传,期间不碰引擎线程。
//!
//! # ABI
//!
//! - 命令:宿主按清单里的命令 id 调用同名导出,输入是参数 JSON,输出是
//!   结果 JSON;导出返回非 0 或设了 extism 错误就是失败。
//! - 外观:宿主调用 `lueying_appearance` 导出,输入是场景 JSON。
//! - 服务:`extism:host/user` 里的 `lueying_<服务>` 收一段 JSON
//!   `{"method":…,"args":…}`,回一段 JSON `{"ok":true,"value":…}`。
//!
//! # 边界
//!
//! - 线性内存上限 [`Limits::memory_bytes`],值栈上限 [`Limits::stack_bytes`]。
//! - 初始化与每次调用都有燃料预算;燃料烧完即 trap,连续 [`MAX_FAULTS`]
//!   次失败停用该包。
//! - 隐私字段([`SceneIn::private`])直接跳过,guest 不会收到那次状态。
//! - 清单 `needs` 里没声明的服务不链接,guest 用到就在实例化时失败。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use extism::{
    CompiledPlugin, CurrentPlugin, Manifest as ExtismManifest, Plugin, PluginBuilder, UserData,
    Val, ValType, Wasm,
};
use lueying_extproto::{MAX_RESULT_BYTES, SceneIn, ScenePatch};
use lueying_extsvc::Registry;

use crate::Manifest;
use crate::manifest::Service;

/// worker 线程的宿主栈;extism 内核、wasm 帧(wasmtime 把
/// [`Limits::stack_bytes`] 当真实线程栈约束)与宿主函数的 serde 解析都要
/// 装下,固定 2 MiB。
const WORKER_STACK: usize = 2 << 20;

/// extism 内核的一页;内存上限按页算。
const PAGE: usize = 65536;

/// 宿主函数的命名空间;extism 内核在 `extism:host/env`,宿主服务在用户区。
const USER_NS: &str = "extism:host/user";

/// 外观导出名;清单 `appearance` 为 true 时必须存在。
const APPEARANCE_EXPORT: &str = "lueying_appearance";

/// 初始化导出名;存在就调一次,失败即加载失败。
const INIT_EXPORT: &str = "lueying_init";

/// 运行期上限。
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// 线性内存上限(字节);换算成 extism 的页数上限。
    pub memory_bytes: usize,
    /// Wasm 值栈上限(字节);wasmtime 用它约束真实线程栈上的 wasm 帧,
    /// 必须明显小于 worker 线程栈(WORKER_STACK)。
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
            dispatches: self.dispatches.load(Ordering::Relaxed),
            steps: self.steps.load(Ordering::Relaxed),
            dispatch_peak_us: self.dispatch_peak_us.load(Ordering::Relaxed),
            step_peak_us: self.step_peak_us.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
            ooms: self.ooms.load(Ordering::Relaxed),
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
    limits: Limits,
    seq: AtomicU64,
}

impl Worker {
    /// 一个扩展的模块起一个 worker;加载失败时返回错误,线程已回收。
    pub(crate) fn spawn(
        wasm: Vec<u8>,
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
                    wasm,
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
        match ready_rx.recv_timeout(Duration::from_secs(30)) {
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

/// 一个 extism 插件实例。
struct Guest {
    plugin: Plugin,
    manifest: Arc<Manifest>,
    metrics: Arc<Metrics>,
}

impl Guest {
    fn new(
        wasm: Vec<u8>,
        manifest: Arc<Manifest>,
        services: Arc<Registry>,
        limits: Limits,
        cancelled: Arc<AtomicBool>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, String> {
        let mut builder = PluginBuilder::new(extism_manifest(&wasm, limits))
            .with_wasi(true)
            .with_cache_disabled()
            .with_fuel_limit(call_fuel(&manifest, limits))
            .with_initialization_fuel_limit(limits.init_fuel)
            .with_wasmtime_config(wasmtime_config(limits));
        for service in &manifest.needs {
            builder = builder.with_function_in_namespace(
                USER_NS,
                service_function_name(*service),
                [ValType::I64],
                [ValType::I64],
                UserData::new(ServiceCtx {
                    caller: manifest.id.clone(),
                    service: *service,
                    services: Arc::clone(&services),
                    cancelled: Arc::clone(&cancelled),
                }),
                service_call,
            );
        }
        builder = builder.with_function_in_namespace(
            USER_NS,
            "lueying_timeout",
            [ValType::I64],
            [ValType::I32],
            UserData::new(TimeoutCtx {
                cancelled: Arc::clone(&cancelled),
            }),
            timeout_call,
        );
        let compiled = CompiledPlugin::new(builder)
            .map_err(|error| format!("扩展 {} 编译失败: {error}", manifest.id))?;
        let plugin = Plugin::new_from_compiled(&compiled)
            .map_err(|error| format!("扩展 {} 加载失败: {error}", explain_link_error(&error)))?;
        for command in manifest.commands() {
            if !plugin.function_exists(&command.id) {
                return Err(format!("扩展 {} 没有导出命令 {}", manifest.id, command.id));
            }
        }
        if manifest.is_appearance() && !plugin.function_exists(APPEARANCE_EXPORT) {
            return Err(format!("扩展 {} 没有导出 {APPEARANCE_EXPORT}", manifest.id));
        }
        if manifest.commands().is_empty() && !manifest.is_appearance() {
            return Err(format!("扩展 {} 没有可调用的入口", manifest.id));
        }
        let mut guest = Self {
            plugin,
            manifest,
            metrics,
        };
        guest.init()?;
        Ok(guest)
    }

    /// 插件导出了初始化入口就调一次;失败即加载失败。
    fn init(&mut self) -> Result<(), String> {
        if !self.plugin.function_exists(INIT_EXPORT) {
            return Ok(());
        }
        self.call(INIT_EXPORT, "")
            .map(|_| ())
            .map_err(|error| format!("扩展 {} 初始化失败: {error}", self.manifest.id))
    }

    fn command(&mut self, id: &str, args: &str) -> Result<String, String> {
        let started = Instant::now();
        let outcome = self.call(id, args);
        self.note(started);
        match outcome {
            Ok(bytes) => {
                String::from_utf8(bytes).map_err(|_| format!("命令 {id} 的结果不是 UTF-8"))
            }
            Err(error) => Err(format!("命令 {id} 执行失败: {error}")),
        }
    }

    fn appearance(&mut self, scene: &SceneIn) -> Result<Option<ScenePatch>, String> {
        let json = scene.to_json()?;
        let started = Instant::now();
        let outcome = self.call(APPEARANCE_EXPORT, &json);
        self.note(started);
        let bytes = outcome.map_err(|error| format!("外观派发失败: {error}"))?;
        let text = String::from_utf8(bytes).map_err(|_| "外观补丁不是 UTF-8".to_string())?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        ScenePatch::parse(&text).map(Some)
    }

    /// 调一个导出;trap 之后实例照旧能用,extism 每次调用都重置内核状态。
    fn call(&mut self, export: &str, input: &str) -> Result<Vec<u8>, extism::Error> {
        let outcome = self.plugin.call::<&[u8], Vec<u8>>(export, input.as_bytes());
        if let Err(error) = &outcome {
            self.classify(error);
        }
        outcome
    }

    /// 把错误归类:燃料耗尽算超时,内存限额算超限。
    fn classify(&self, error: &extism::Error) {
        let text = error.to_string().to_ascii_lowercase();
        if text.contains("fuel") {
            self.metrics.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        if text.contains("oom") || text.contains("memory") || text.contains("grow") {
            self.metrics.ooms.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn note(&mut self, started: Instant) {
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let used = self.plugin.fuel_consumed().unwrap_or(0);
        self.metrics.dispatches.fetch_add(1, Ordering::Relaxed);
        self.metrics.steps.fetch_add(used, Ordering::Relaxed);
        self.metrics
            .dispatch_peak_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
        self.metrics
            .step_peak_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
    }
}

/// 造一份 extism 清单:模块、内存页上限与联网策略。
///
/// 联网一律走宿主 broker:extism 自己不放行任何域名,清单里的
/// `network.allow` 由宿主的服务提供者逐请求强制。
fn extism_manifest(wasm: &[u8], limits: Limits) -> ExtismManifest {
    let mut out = ExtismManifest::new([Wasm::data(wasm.to_vec())]).disallow_all_hosts();
    out.memory.max_pages = Some((limits.memory_bytes / PAGE).max(1) as u32);
    out
}

/// 每次调用的燃料预算;只有外观的包按帧算,带命令的包按命令算。
fn call_fuel(manifest: &Manifest, limits: Limits) -> u64 {
    if manifest.commands().is_empty() {
        limits.appearance_fuel
    } else {
        limits.command_fuel
    }
}

/// wasmtime 配置;栈上限按 [`Limits`] 配,其余用 extism 的默认。
fn wasmtime_config(limits: Limits) -> wasmtime::Config {
    let mut config = wasmtime::Config::default();
    config.max_wasm_stack(limits.stack_bytes.max(64 << 10));
    config
}

/// 服务在 extism 用户区的函数名。
fn service_function_name(service: Service) -> String {
    format!("lueying_{}", service.key())
}

/// 把链接错误翻译成人话:扩展用了清单没声明的宿主服务。
fn explain_link_error(error: &extism::Error) -> String {
    let text = error.to_string();
    for service in Service::ALL {
        if text.contains(&service_function_name(service)) {
            return format!(
                "用到了没声明的宿主服务 {}(清单 needs 里补上)",
                service.key()
            );
        }
    }
    text
}

/// 服务宿主函数的上下文。
struct ServiceCtx {
    /// 调用方扩展 id;审计与按扩展隔离的存储用。
    caller: String,
    /// 服务。
    service: Service,
    /// 服务提供者表。
    services: Arc<Registry>,
    /// 取消标志;扩展卸载时置位。
    cancelled: Arc<AtomicBool>,
}

/// 一次服务调用的请求;guest 发来的 JSON。
#[derive(serde::Deserialize)]
struct ServiceRequest {
    /// 方法名;宿主按服务白名单核。
    method: String,
    /// 参数;原样转给提供者。
    #[serde(default)]
    args: serde_json::Value,
}

/// 可取消等待的上下文。
struct TimeoutCtx {
    /// 取消标志;扩展卸载时置位。
    cancelled: Arc<AtomicBool>,
}

/// 宿主服务:收一段 JSON 请求,回一段 JSON 响应。
///
/// 请求是 `{"method":…,"args":…}`;响应是 `{"ok":true,"value":…}` 或
/// `{"ok":false,"error":…}`。返回值是 extism 内存里那块响应的偏移。
fn service_call(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<ServiceCtx>,
) -> Result<(), extism::Error> {
    let ctx = user_data.get()?;
    let ctx = ctx
        .lock()
        .map_err(|_| extism::Error::msg("服务上下文锁坏了"))?;
    let offset = inputs.first().and_then(Val::i64).unwrap_or(0).max(0) as u64;
    let raw = read_block(plugin, offset)?;
    let request: ServiceRequest = serde_json::from_slice(&raw)
        .map_err(|error| extism::Error::msg(format!("服务请求解析失败: {error}")))?;
    let timeout = timeout_for(ctx.service, &request.method);
    let args = match &request.args {
        serde_json::Value::Null => "{}".to_string(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let result = ctx.services.call_cancellable(
        &ctx.caller,
        ctx.service.key(),
        &request.method,
        &args,
        timeout,
        Some(&ctx.cancelled),
    );
    let envelope = match result {
        Ok(value) => {
            let value = serde_json::from_str::<serde_json::Value>(&value)
                .unwrap_or(serde_json::Value::String(value));
            serde_json::json!({ "ok": true, "value": value })
        }
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    let bytes = serde_json::to_vec(&envelope)
        .map_err(|error| extism::Error::msg(format!("服务响应序列化失败: {error}")))?;
    let handle = plugin.memory_alloc(bytes.len() as u64)?;
    plugin.memory_bytes_mut(handle)?.copy_from_slice(&bytes);
    outputs[0] = plugin.memory_to_val(handle);
    Ok(())
}

/// 宿主侧 `lueying_timeout(ms)`:可取消的等待,卸载时 50 ms 内返回。
fn timeout_call(
    _plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<TimeoutCtx>,
) -> Result<(), extism::Error> {
    let ctx = user_data.get()?;
    let ctx = ctx
        .lock()
        .map_err(|_| extism::Error::msg("等待上下文锁坏了"))?;
    let mut left = inputs
        .first()
        .and_then(Val::i64)
        .unwrap_or(0)
        .clamp(0, 600_000) as u64;
    while left > 0 {
        if ctx.cancelled.load(Ordering::Relaxed) {
            outputs[0] = Val::I32(-1);
            return Ok(());
        }
        let step = left.min(50);
        std::thread::sleep(Duration::from_millis(step));
        left -= step;
    }
    let cancelled = ctx.cancelled.load(Ordering::Relaxed);
    outputs[0] = Val::I32(if cancelled { -1 } else { 0 });
    Ok(())
}

/// 从 extism 内存读一块数据。
fn read_block(plugin: &mut CurrentPlugin, offset: u64) -> Result<Vec<u8>, extism::Error> {
    if offset == 0 {
        return Ok(Vec::new());
    }
    let handle = plugin
        .memory_handle(offset)
        .ok_or_else(|| extism::Error::msg("宿主服务收到无效的内存偏移"))?;
    let bytes = plugin.memory_bytes(handle)?;
    if bytes.len() > MAX_RESULT_BYTES {
        return Err(extism::Error::msg(format!(
            "服务请求超过 {MAX_RESULT_BYTES} 字节"
        )));
    }
    Ok(bytes.to_vec())
}

/// 服务的宿主等待时限。面板要等用户操作,识别要跑满传输时限,单独放宽。
fn timeout_for(service: Service, method: &str) -> Duration {
    match (service, method) {
        (Service::Panel, "show") => Duration::from_secs(600),
        (Service::Ocr, "recognize") => Duration::from_secs(120),
        (Service::Capture, "shot") => Duration::from_secs(30),
        (Service::Network, "fetch") => Duration::from_secs(140),
        _ => Duration::from_secs(10),
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
