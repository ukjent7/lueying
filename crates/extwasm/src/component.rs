//! 组件宿主:WASI 0.3 组件模型的原生绑定(wasm ABI v2)。
//!
//! 载入判定不看清单:wasm 字节带组件魔数(版本段 0x0d)的走这里,核心模块
//! 魔数的走 Extism 宿主(四个官方扩展迁移期内的桥)。安全语义与 v1 一致:
//! 清单 needs 里没声明的服务不进 linker,扩展用到就在实例化时失败;燃料按
//! 初始化/命令两段预算,耗尽即 trap;宿主服务阻塞在扩展自己的 worker 线程
//! 上,失败以 result 的 Err 分支回给扩展并记入失败槽(吞错保护)。
//!
//! 绑定全部由 [`bindgen!`] 从 `lueying.wit` 生成:导入接口按包路径镜像
//! (`lueying:ext/clipboard` → [`lueying::ext::clipboard`]),世界级导出
//! ([`Extension`])在根。生成的 record 带 serde 派生,桥接层走 JSON 值与
//! [`Registry`] 互通,与 v1 的 JSON 契约逐字节兼容。

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, Store};

use lueying_extmeta::{Manifest, Service};
use lueying_extproto::{SceneIn, ScenePatch};

use crate::host::{Limits, call_fuel, timeout_for, wasmtime_config};
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
}

impl ComponentGuest {
    /// 装载组件并跑 init;燃料按初始化预算。
    pub(crate) fn new(
        wasm: Vec<u8>,
        manifest: Arc<Manifest>,
        services: Arc<Registry>,
        limits: Limits,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let state = GuestState {
            caller: manifest.id.clone(),
            manifest: Arc::clone(&manifest),
            services,
            cancelled,
            failure: Arc::new(Mutex::new(None)),
        };
        let mut config = wasmtime_config(limits);
        config.wasm_component_model(true);
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|error| format!("组件引擎启动失败: {error}"))?;
        let component =
            Component::new(&engine, &wasm).map_err(|error| format!("组件装载失败: {error}"))?;
        let mut store = Store::new(&engine, state);
        store
            .set_fuel(limits.init_fuel)
            .map_err(|error| format!("初始化燃料设置失败: {error}"))?;

        // 只有清单 needs 里声明的服务才进 linker;guest 用到未声明的服务
        // 就在实例化时报错,与 v1 的链接期语义一致。
        let mut linker = Linker::new(&engine);
        for service in &manifest.needs {
            match service.key() {
                "capture" => {
                    lueying::ext::capture::add_to_linker::<GuestState, HasSelf<GuestState>>(
                        &mut linker,
                        |state| state,
                    )
                    .map_err(|error| format!("服务 capture 链接失败: {error}"))?
                }
                "ocr" => lueying::ext::ocr::add_to_linker::<GuestState, HasSelf<GuestState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| format!("服务 ocr 链接失败: {error}"))?,
                "clipboard" => lueying::ext::clipboard::add_to_linker::<
                    GuestState,
                    HasSelf<GuestState>,
                >(&mut linker, |state| state)
                .map_err(|error| format!("服务 clipboard 链接失败: {error}"))?,
                "store" => lueying::ext::store::add_to_linker::<GuestState, HasSelf<GuestState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| format!("服务 store 链接失败: {error}"))?,
                "output" => lueying::ext::output::add_to_linker::<GuestState, HasSelf<GuestState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| format!("服务 output 链接失败: {error}"))?,
                "panel" => lueying::ext::panel::add_to_linker::<GuestState, HasSelf<GuestState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| format!("服务 panel 链接失败: {error}"))?,
                "hotkey" => lueying::ext::hotkey::add_to_linker::<GuestState, HasSelf<GuestState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| format!("服务 hotkey 链接失败: {error}"))?,
                "notify" => lueying::ext::notify::add_to_linker::<GuestState, HasSelf<GuestState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| format!("服务 notify 链接失败: {error}"))?,
                "network" => {
                    lueying::ext::network::add_to_linker::<GuestState, HasSelf<GuestState>>(
                        &mut linker,
                        |state| state,
                    )
                    .map_err(|error| format!("服务 network 链接失败: {error}"))?
                }
                "context" => {
                    lueying::ext::context::add_to_linker::<GuestState, HasSelf<GuestState>>(
                        &mut linker,
                        |state| state,
                    )
                    .map_err(|error| format!("服务 context 链接失败: {error}"))?
                }
                "audio" => lueying::ext::audio::add_to_linker::<GuestState, HasSelf<GuestState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| format!("服务 audio 链接失败: {error}"))?,
                "asr" => lueying::ext::asr::add_to_linker::<GuestState, HasSelf<GuestState>>(
                    &mut linker,
                    |state| state,
                )
                .map_err(|error| format!("服务 asr 链接失败: {error}"))?,
                other => return Err(format!("未知服务 {other:?}")),
            }
        }
        let bindings = Extension::instantiate(&mut store, &component, &linker)
            .map_err(|error| format!("组件实例化失败: {error}"))?;
        let init = bindings
            .entry()
            .call_init(&mut store)
            .map_err(|error| format!("扩展 init 失败: {error}"))?;
        init.map_err(|error| format!("扩展 init 返回错误: {error}"))?;
        Ok(Self {
            store,
            bindings,
            manifest,
            limits,
        })
    }

    /// 跑一条命令;燃料按命令预算,服务失败经失败槽判死,不被吞掉。
    pub(crate) fn command(&mut self, id: &str, args: &str) -> Result<String, String> {
        self.store
            .set_fuel(call_fuel(&self.manifest, self.limits))
            .map_err(|error| format!("命令燃料设置失败: {error}"))?;
        let result = self
            .bindings
            .entry()
            .call_command(&mut self.store, id, args)
            .map_err(|error| format!("扩展命令执行失败: {error}"))?;
        self.harvest_failure(result)
    }

    /// 跑一帧外观;`None` 表示这一帧不改。
    pub(crate) fn appearance(&mut self, scene: &SceneIn) -> Result<Option<ScenePatch>, String> {
        self.store
            .set_fuel(call_fuel(&self.manifest, self.limits))
            .map_err(|error| format!("外观燃料设置失败: {error}"))?;
        let scene_json =
            serde_json::to_string(scene).map_err(|error| format!("场景序列化失败: {error}"))?;
        let result = self
            .bindings
            .entry()
            .call_appearance(&mut self.store, &scene_json)
            .map_err(|error| format!("扩展外观执行失败: {error}"))?;
        let json = self.harvest_failure(result)?;
        json.map(|json| serde_json::from_str::<ScenePatch>(&json))
            .transpose()
            .map_err(|error| format!("外观补丁解析失败: {error}"))
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
