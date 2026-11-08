//! 组件宿主:WASI 0.3 组件模型的原生绑定(wasm ABI v2)。
//!
//! 扩展必须是标准 WebAssembly 组件(核心模块用 `wasm-tools component new`
//! 打包);安全语义:12 个服务接口全量链接(非 Rust 工具链不裁剪导入),
//! 是否放行按调用逐次核清单 needs,没声明的服务一调即拒;燃料按初始化/
//! 命令两段预算,耗尽即 trap;线性内存有上限。宿主服务阻塞在扩展自己的
//! worker 线程上,失败以 result 的 Err 分支回给扩展并记入失败槽(吞错
//! 保护)。
//!
//! 绑定全部由 [`bindgen!`] 从 `lueying.wit` 生成:导入接口按包路径镜像
//! (`lueying:ext/clipboard` → [`lueying::ext::clipboard`]),世界级导出
//! ([`Extension`])在根。生成的 record 带 serde 派生,桥接层走 JSON 值与
//! [`Registry`] 互通,与 v1 的 JSON 契约逐字节兼容。
//!
//! WASI 0.2 也挂在 linker 上,给非 Rust 工具链的组件用;能力全关(无文件、
//! 无网络、无环境变量、无标准流),只有时钟与随机。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Engine, ResourceLimiter, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use lueying_extmeta::{Manifest, Service};
use lueying_extproto::{SceneIn, ScenePatch};

use crate::host::{Limits, Metrics, call_fuel, timeout_for, wasmtime_config};
use crate::services::Registry;

wasmtime::component::bindgen!({
    path: "lueying.wit",
    world: "extension",
    additional_derives: [serde::Serialize, serde::Deserialize],
});

/// 组件 guest 的宿主状态:每个扩展一份,12 个服务接口的实现都挂在它上面。
pub struct GuestState {
    caller: String,
    manifest: Arc<Manifest>,
    services: Arc<Registry>,
    cancelled: Arc<AtomicBool>,
    /// 本次调用里失败的服务;guest 仍返回成功时用它把这次调用判失败。
    failure: Arc<Mutex<Option<String>>>,
    /// 线性内存限额;拒绝增长时记一笔,指标据此把 OOM 与普通 trap 分开。
    limiter: GuestLimiter,
    /// WASI 0.2 状态:非 Rust 工具链的组件会导入;能力全关,只有时钟与随机。
    table: ResourceTable,
    wasi: WasiCtx,
}

impl WasiView for GuestState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl GuestState {
    /// 服务调用的公共路径:核清单声明、按服务与方法取时限、失败记槽。
    fn call(&mut self, service: &str, method: &str, args: &str) -> Result<String, String> {
        let service_enum = Service::ALL
            .iter()
            .copied()
            .find(|s| s.key() == service)
            .ok_or_else(|| format!("未知服务 {service:?}"))?;
        if !self.manifest.needs.contains(&service_enum) {
            return Err(format!("服务 {service} 不在清单 needs 里(声明了才可用)"));
        }
        let timeout = timeout_for(service_enum, method);
        let result = self.services.call_cancellable(
            &self.caller,
            service,
            method,
            args,
            timeout,
            Some(&self.cancelled),
        );
        if let Err(error) = &result
            && let Ok(mut slot) = self.failure.lock()
        {
            *slot = Some(error.clone());
        }
        result
    }

    fn parse<T: serde::de::DeserializeOwned>(value: &str, what: &str) -> Result<T, String> {
        serde_json::from_str(value).map_err(|error| format!("{what}解析失败: {error}"))
    }

    fn args<T: serde::Serialize>(options: &T) -> Result<String, String> {
        serde_json::to_string(options).map_err(|error| format!("参数序列化失败: {error}"))
    }
}

// 桥接:12 个服务接口,生成类型 ↔ Registry 的 JSON 契约。

impl lueying::ext::capture::Host for GuestState {
    fn shot(
        &mut self,
        options: lueying::ext::capture::ShotOptions,
    ) -> Result<lueying::ext::capture::ShotResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("capture", "shot", &args)?;
        Self::parse(&value, "截图结果")
    }
}

impl lueying::ext::ocr::Host for GuestState {
    fn recognize(
        &mut self,
        options: lueying::ext::ocr::RecognizeOptions,
    ) -> Result<lueying::ext::ocr::RecognizeResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("ocr", "recognize", &args)?;
        Self::parse(&value, "识别结果")
    }
}

impl lueying::ext::clipboard::Host for GuestState {
    fn read(&mut self) -> Result<lueying::ext::clipboard::ReadResult, String> {
        let value = self.call("clipboard", "read", "{}")?;
        Self::parse(&value, "剪贴板结果")
    }

    fn write(&mut self, options: lueying::ext::clipboard::WriteOptions) -> Result<String, String> {
        let args = Self::args(&options)?;
        self.call("clipboard", "write", &args)
    }

    fn history(
        &mut self,
        options: lueying::ext::clipboard::HistoryOptions,
    ) -> Result<lueying::ext::clipboard::HistoryResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("clipboard", "history", &args)?;
        Self::parse(&value, "剪贴板历史")
    }

    fn remember(
        &mut self,
        options: lueying::ext::clipboard::RememberOptions,
    ) -> Result<String, String> {
        let args = Self::args(&options)?;
        self.call("clipboard", "remember", &args)
    }

    fn clear(&mut self) -> Result<String, String> {
        self.call("clipboard", "clear", "{}")
    }
}

impl lueying::ext::store::Host for GuestState {
    fn get(
        &mut self,
        options: lueying::ext::store::GetOptions,
    ) -> Result<lueying::ext::store::GetResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("store", "get", &args)?;
        Self::parse(&value, "存储读取")
    }

    fn set(&mut self, options: lueying::ext::store::SetOptions) -> Result<String, String> {
        let args = Self::args(&options)?;
        self.call("store", "set", &args)
    }

    fn remove(&mut self, options: lueying::ext::store::RemoveOptions) -> Result<String, String> {
        let args = Self::args(&options)?;
        self.call("store", "remove", &args)
    }

    fn list(&mut self) -> Result<lueying::ext::store::ListResult, String> {
        let value = self.call("store", "list", "{}")?;
        Self::parse(&value, "存储键表")
    }
}

impl lueying::ext::output::Host for GuestState {
    fn deliver(
        &mut self,
        options: lueying::ext::output::DeliverOptions,
    ) -> Result<lueying::ext::output::DeliverResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("output", "deliver", &args)?;
        Self::parse(&value, "上屏结果")
    }
}

impl lueying::ext::panel::Host for GuestState {
    fn show(
        &mut self,
        options: lueying::ext::panel::ShowOptions,
    ) -> Result<lueying::ext::panel::ShowResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("panel", "show", &args)?;
        Self::parse(&value, "面板结果")
    }
}

impl lueying::ext::hotkey::Host for GuestState {
    fn register(
        &mut self,
        options: lueying::ext::hotkey::RegisterOptions,
    ) -> Result<String, String> {
        let args = Self::args(&options)?;
        self.call("hotkey", "register", &args)
    }

    fn unregister(
        &mut self,
        options: lueying::ext::hotkey::UnregisterOptions,
    ) -> Result<String, String> {
        let args = Self::args(&options)?;
        self.call("hotkey", "unregister", &args)
    }

    fn press(&mut self, options: lueying::ext::hotkey::PressOptions) -> Result<String, String> {
        let args = Self::args(&options)?;
        self.call("hotkey", "press", &args)
    }
}

impl lueying::ext::notify::Host for GuestState {
    fn show(&mut self, options: lueying::ext::notify::ShowOptions) -> Result<String, String> {
        let args = Self::args(&options)?;
        self.call("notify", "show", &args)
    }
}

impl lueying::ext::network::Host for GuestState {
    fn fetch(
        &mut self,
        options: lueying::ext::network::FetchOptions,
    ) -> Result<lueying::ext::network::FetchResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("network", "fetch", &args)?;
        Self::parse(&value, "联网结果")
    }
}

impl lueying::ext::context::Host for GuestState {
    fn selection(&mut self) -> Result<lueying::ext::context::SelectionResult, String> {
        let value = self.call("context", "selection", "{}")?;
        Self::parse(&value, "选中内容")
    }
}

impl lueying::ext::audio::Host for GuestState {
    fn record(
        &mut self,
        options: lueying::ext::audio::RecordOptions,
    ) -> Result<lueying::ext::audio::RecordResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("audio", "record", &args)?;
        Self::parse(&value, "录音结果")
    }
}

impl lueying::ext::asr::Host for GuestState {
    fn recognize(
        &mut self,
        options: lueying::ext::asr::RecognizeOptions,
    ) -> Result<lueying::ext::asr::RecognizeResult, String> {
        let args = Self::args(&options)?;
        let value = self.call("asr", "recognize", &args)?;
        Self::parse(&value, "语音识别结果")
    }
}

/// 一个组件扩展的常驻句柄:引擎、存储与类型化绑定都归 worker 线程所有。
pub(crate) struct ComponentGuest {
    store: Store<GuestState>,
    bindings: Extension,
    manifest: Arc<Manifest>,
    limits: Limits,
    metrics: Arc<Metrics>,
}

impl ComponentGuest {
    /// 装载组件并跑 init;燃料按初始化预算。
    ///
    /// 引擎与组件由调用方先行编译(见 [`build_engine`]/[`build_component`]),
    /// 这里只做实例化与 init,受 30 秒就绪窗口约束;init 的 CPU 有燃料
    /// 封顶,不会无限等。
    pub(crate) fn new(
        engine: Engine,
        component: Component,
        manifest: Arc<Manifest>,
        services: Arc<Registry>,
        limits: Limits,
        cancelled: Arc<AtomicBool>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, String> {
        let state = GuestState {
            caller: manifest.id.clone(),
            manifest: Arc::clone(&manifest),
            services,
            cancelled,
            failure: Arc::new(Mutex::new(None)),
            limiter: GuestLimiter {
                memory_bytes: limits.memory_bytes,
                denied: false,
            },
            table: ResourceTable::new(),
            wasi: WasiCtx::builder().build(),
        };
        let mut store = Store::new(&engine, state);
        store.limiter(|state| &mut state.limiter);
        store
            .set_fuel(limits.init_fuel)
            .map_err(|error| format!("初始化燃料设置失败: {error}"))?;

        // 12 个服务接口全部进 linker:非 Rust 工具链(componentize-qjs、
        // componentize-py)按整个世界生成导入,不做裁剪。是否放行不看
        // 链接看调用——GuestState::call 按清单 needs 逐次门控,没声明的
        // 服务一调即拒,安全语义不变。
        let mut linker = Linker::new(&engine);
        lueying::ext::capture::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 capture 链接失败: {error}"))?;
        lueying::ext::ocr::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 ocr 链接失败: {error}"))?;
        lueying::ext::clipboard::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 clipboard 链接失败: {error}"))?;
        lueying::ext::store::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 store 链接失败: {error}"))?;
        lueying::ext::output::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 output 链接失败: {error}"))?;
        lueying::ext::panel::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 panel 链接失败: {error}"))?;
        lueying::ext::hotkey::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 hotkey 链接失败: {error}"))?;
        lueying::ext::notify::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 notify 链接失败: {error}"))?;
        lueying::ext::network::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 network 链接失败: {error}"))?;
        lueying::ext::context::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 context 链接失败: {error}"))?;
        lueying::ext::audio::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 audio 链接失败: {error}"))?;
        lueying::ext::asr::add_to_linker::<GuestState, HasSelf<GuestState>>(
            &mut linker,
            |state| state,
        )
        .map_err(|error| format!("服务 asr 链接失败: {error}"))?;
        // 非 Rust 工具链(componentize-py、带 stdio 的 JS 等)编出的组件会
        // 导入 WASI 0.2。WASI 不进清单 needs 门控:导入即可用,但能力全关——
        // 无参数、无环境变量、无文件、无网络、无标准流,只有时钟与随机;
        // 联网仍只能走宿主的 network 服务(经清单白名单逐请求强制)。
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
            .map_err(|error| format!("WASI 链接失败: {error}"))?;
        let bindings = Extension::instantiate(&mut store, &component, &linker)
            .map_err(|error| format!("组件实例化失败: {}", explain_link_error(&error.to_string())))?;
        let init = match bindings.entry().call_init(&mut store) {
            Ok(init) => init,
            Err(error) => return Err(init_error(&mut store, error, limits.init_fuel)),
        };
        init.map_err(|error| format!("扩展 init 返回错误: {error}"))?;
        Ok(Self {
            store,
            bindings,
            manifest,
            limits,
            metrics,
        })
    }

    /// 跑一条命令;燃料按命令预算,服务失败经失败槽判死,不被吞掉。
    pub(crate) fn command(&mut self, id: &str, args: &str) -> Result<String, String> {
        let budget = call_fuel(&self.manifest, self.limits);
        self.store
            .set_fuel(budget)
            .map_err(|error| format!("命令燃料设置失败: {error}"))?;
        let started = Instant::now();
        let outcome = self
            .bindings
            .entry()
            .call_command(&mut self.store, id, args)
            .map_err(|error| format!("扩展命令执行失败: {error}"))
            // 外层是 trap,内层是 WIT result;展平后统一按结果结算。
            .flatten();
        self.note(started, budget, outcome.as_ref().err());
        self.harvest_failure(outcome)
    }

    /// 跑一帧外观;`None` 表示这一帧不改。
    pub(crate) fn appearance(&mut self, scene: &SceneIn) -> Result<Option<ScenePatch>, String> {
        let budget = call_fuel(&self.manifest, self.limits);
        self.store
            .set_fuel(budget)
            .map_err(|error| format!("外观燃料设置失败: {error}"))?;
        let scene_json =
            serde_json::to_string(scene).map_err(|error| format!("场景序列化失败: {error}"))?;
        let started = Instant::now();
        let outcome = self
            .bindings
            .entry()
            .call_appearance(&mut self.store, &scene_json)
            .map_err(|error| format!("扩展外观执行失败: {error}"))
            .flatten();
        self.note(started, budget, outcome.as_ref().err());
        let json = self.harvest_failure(outcome)?;
        json.map(|json| ScenePatch::parse(&json))
            .transpose()
            .map_err(|error| format!("外观补丁解析失败: {error}"))
    }

    /// 记一次派发的指标:次数、燃料步数、耗时峰值;燃料 trap 记超时,
    /// 内存限额拒绝记 OOM。
    fn note(&mut self, started: Instant, budget: u64, error: Option<&String>) {
        let left = self.store.get_fuel().unwrap_or(budget);
        let used = budget.saturating_sub(left);
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let metrics = Arc::clone(&self.metrics);
        metrics.dispatches.fetch_add(1, Ordering::Relaxed);
        metrics.steps.fetch_add(used, Ordering::Relaxed);
        metrics
            .dispatch_peak_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
        metrics
            .step_peak_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
        // 燃料耗尽的 trap 只带 wasm backtrace 不带文字,按「失败且剩余燃料
        // 为零」归类为超时;其余 trap(如 panic)剩余燃料还有剩。
        if error.is_some() && left == 0 {
            metrics.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        if self.store.data().limiter.denied {
            self.store.data_mut().limiter.denied = false;
            metrics.ooms.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 命令成功且失败槽有值 → 判失败;命令本身 Err → 原样透传。
    fn harvest_failure<T>(&mut self, result: Result<T, String>) -> Result<T, String> {
        let failure = self.store.data().failure.lock().expect("失败槽锁").take();
        match result {
            Ok(_value) if failure.is_some() => Err(failure.unwrap()),
            other => other,
        }
    }
}

/// 载入判定:组件魔数(版本段 0x0d)为真。
pub(crate) fn is_component(wasm: &[u8]) -> bool {
    wasm.len() >= 8 && wasm[4..8] == [0x0d, 0x00, 0x01, 0x00]
}

/// 按预算建 wasm 引擎;编译前的调用方步骤,失败即加载失败。
pub(crate) fn build_engine(limits: Limits) -> Result<Engine, String> {
    let mut config = wasmtime_config(limits);
    config.wasm_component_model(true);
    config.consume_fuel(true);
    Engine::new(&config).map_err(|error| format!("组件引擎启动失败: {error}"))
}

/// 编译组件;大组件(JS 引擎、CPython)可能耗时数分钟,放调用线程做,
/// 不占 worker 的就绪窗口。
pub(crate) fn build_component(engine: &Engine, wasm: &[u8]) -> Result<Component, String> {
    if !is_component(wasm) {
        return Err("不是 WebAssembly 组件:核心模块要用 wasm-tools component new 打包".into());
    }
    Component::new(engine, wasm).map_err(|error| format!("组件装载失败: {error}"))
}

/// init 失败的人话:燃料烧尽单独点名,其余带上剩余燃料方便调预算。
fn init_error(store: &mut Store<GuestState>, error: wasmtime::Error, budget: u64) -> String {
    match store.get_fuel() {
        Ok(0) => format!("扩展 init 失败:init 燃料耗尽(预算 {budget})"),
        Ok(left) => format!("扩展 init 失败(剩余燃料 {left}): {error}"),
        Err(_) => format!("扩展 init 失败: {error}"),
    }
}

/// 把实例化错误翻译成人话:契约不符(版本不对、缺导出)给指路。
/// 服务接口已全量链接,再出现 `lueying:ext/<服务>` 的链接错误只会是
/// 契约版本不匹配。
fn explain_link_error(text: &str) -> String {
    for service in Service::ALL {
        if text.contains(&format!("lueying:ext/{}", service.key())) {
            return format!(
                "扩展契约与宿主不符(服务 {}):组件要用 lueying:ext@2.0.0 编译",
                service.key()
            );
        }
    }
    text.to_string()
}

/// 线性内存限额:只管内存,增长越过上限就拒绝并记一笔。
struct GuestLimiter {
    memory_bytes: usize,
    /// 上次派发里是否有被拒绝的增长;[`ComponentGuest::note`] 取走清零。
    denied: bool,
}

impl ResourceLimiter for GuestLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        let ok = desired <= self.memory_bytes;
        self.denied |= !ok;
        Ok(ok)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        Ok(true)
    }
}
