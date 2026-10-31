//! 运行期 Wasm 扩展层原型:组件宿主与嵌入成本测量。
//!
//! 任务书 T14 的 Wasm 半边。预算语义与脚本层(`lueying-extjs`)对齐:
//! 燃料按调用计费(确定性预算)、epoch 当墙钟兜底(宿主泵循环推进)、
//! 内存上限走 ResourceLimiter、连续失败停用;接口契约记录在
//! `wit/lueying.wit`,本原型接平坦标量的 `probe` 世界,重排世界等测量
//! 结论。测量本身在 e2e 的 wasm 场景,数据进 CI 报告后再决定 Wasm 是
//! 否成为默认第三方形态。
//!
//! 宿主不加载任何未签名 DLL 的原则不变:Wasm 组件是数据,编译产物在
//! 宿主内的沙箱里执行,进程边界与能力模型照旧。

use std::sync::Arc;
use std::time::Instant;

use wasmtime::component::{Component, Linker, TypedFunc};
use wasmtime::{Config, Engine as WasmEngine, Store};

/// 原型探针组件:模块内导出 `add(a, b) -> a + b`,线性内存为零。
/// e2e 测量与单元测试都用它;正式重排组件由外部工具链产出二进制。
pub const PROBE_WAT: &str = r#"
(component
  (core module $m
    (func (export "add") (param i32 i32) (result i32)
      local.get 0
      local.get 1
      i32.add))
  (core instance $i (instantiate $m))
  (type (func (param "a" u32) (param "b" u32) (result u32)))
  (func (export "add") (type 0) (canon lift (core func $i "add")))
)"#;

/// 单模块内存上限,与脚本层一致。
pub const MEMORY_BYTES: usize = 8 * 1024 * 1024;
/// 表条目上限;重排模块用不到大表。
pub const TABLE_ENTRIES: usize = 10_000;
/// 实例化预算:canon lift 与模块初始化的固定开销。
const INSTANTIATE_FUEL: u64 = 2_000_000;
/// 单次调用燃料预算;烧完即故障,连续超限停用。
pub const CALL_FUEL: u64 = 200_000;
/// epoch 兜底预算:宿主每秒推进 epoch,ticks 内不结束按超时算。
pub const EPOCH_CALL_TICKS: u64 = 100;
/// 连续失败几次后停用,与脚本层一致。
pub const MAX_FAULTS: u32 = 3;

/// 组件宿主。Engine 可跨模块共享;epoch 时钟也挂在它上面。
pub struct WasmHost {
    engine: WasmEngine,
}

impl Default for WasmHost {
    fn default() -> Self {
        Self::new().expect("Wasmtime 引擎默认配置应可用")
    }
}

impl WasmHost {
    pub fn new() -> Result<Self, String> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = WasmEngine::new(&config).map_err(|error| format!("引擎创建失败: {error}"))?;
        Ok(Self { engine })
    }

    /// 宿主泵循环里周期调用:推进 epoch 时钟,墙钟兜底才生效。
    pub fn tick(&self) {
        self.engine.increment_epoch();
    }
}

/// 内存与表上限;拒绝方式按 WebAssembly 规范回 -1,不是陷阱。
struct MemLimiter;

impl wasmtime::ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        Ok(desired <= MEMORY_BYTES)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        Ok(desired <= TABLE_ENTRIES)
    }
}

/// Store 的宿主状态;ResourceLimiter 钩子从这里借。
struct Guest {
    limiter: MemLimiter,
}

/// 一次调用的统计;测量与「连续失败停用」都读它。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WasmStat {
    pub compile_ms: u64,
    pub instantiate_ms: u64,
    pub call_peak_us: u64,
    /// 最近一次调用烧掉的燃料;正常往返也记录,给测量用。
    pub fuel_used: u64,
    pub faults: u32,
    pub disabled: bool,
    pub last_error: Option<String>,
}

/// 已编译的组件模块;热重载由调用方按内容哈希丢旧建新。
pub struct WasmModule {
    host: Arc<WasmHost>,
    component: Component,
    stat: WasmStat,
}

/// `expect_err` 一类断言要求成功类型可调试;内部句柄没有可读字段可打。
impl std::fmt::Debug for WasmModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmModule").finish_non_exhaustive()
    }
}

impl WasmModule {
    /// 编译组件;二进制与 WAT 文本都收(wat 特性)。
    pub fn compile(host: &Arc<WasmHost>, source: &[u8]) -> Result<Self, String> {
        let started = Instant::now();
        let component = Component::new(&host.engine, source)
            .map_err(|error| format!("组件编译失败: {error}"))?;
        let stat = WasmStat {
            compile_ms: started.elapsed().as_millis() as u64,
            ..WasmStat::default()
        };
        Ok(Self {
            host: Arc::clone(host),
            component,
            stat,
        })
    }

    /// 探针实例:按名字取一个 `(u32, u32) -> u32` 的导出。
    pub fn instantiate(&self, export: &str) -> Result<Probe, String> {
        let started = Instant::now();
        let mut store = Store::new(
            &self.host.engine,
            Guest {
                limiter: MemLimiter,
            },
        );
        store.limiter(|guest: &mut Guest| &mut guest.limiter);
        store
            .set_fuel(INSTANTIATE_FUEL)
            .map_err(|error| format!("燃料初始化失败: {error}"))?;
        // epoch_interruption 打开后必须先给截止期;u64::MAX 等于等宿主
        // tick 推进,原型用燃料做确定性兜底。
        store.set_epoch_deadline(u64::MAX);
        let linker = Linker::<Guest>::new(&self.host.engine);
        let instance = linker
            .instantiate(&mut store, &self.component)
            .map_err(|error| format!("组件实例化失败: {error}"))?;
        let func: TypedFunc<(u32, u32), (u32,)> = instance
            .get_typed_func(&mut store, export)
            .map_err(|error| format!("组件没有导出 {export:?}: {error}"))?;
        let mut stat = self.stat.clone();
        stat.instantiate_ms = started.elapsed().as_millis() as u64;
        Ok(Probe {
            store,
            func,
            stat,
            export: export.to_string(),
        })
    }

    pub fn host(&self) -> &Arc<WasmHost> {
        &self.host
    }

    pub fn stat(&self) -> &WasmStat {
        &self.stat
    }
}

/// 一个可调用的探针实例。按调用计燃料,连续故障停用。
pub struct Probe {
    store: Store<Guest>,
    func: TypedFunc<(u32, u32), (u32,)>,
    stat: WasmStat,
    export: String,
}

impl std::fmt::Debug for Probe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probe")
            .field("export", &self.export)
            .finish_non_exhaustive()
    }
}

impl Probe {
    pub fn call(&mut self, a: u32, b: u32) -> Result<u32, String> {
        if self.stat.disabled {
            return Err(format!(
                "模块已停用(连续 {MAX_FAULTS} 次失败): {}",
                self.stat.last_error.as_deref().unwrap_or("未知错误")
            ));
        }
        self.store
            .set_fuel(CALL_FUEL)
            .map_err(|error| format!("燃料设置失败: {error}"))?;
        self.store.set_epoch_deadline(EPOCH_CALL_TICKS);
        let started = Instant::now();
        let outcome = self.func.call(&mut self.store, (a, b));
        let elapsed_us = started.elapsed().as_micros() as u64;
        self.stat.fuel_used = CALL_FUEL.saturating_sub(self.store.get_fuel().unwrap_or(0));
        match outcome {
            Ok(value) => {
                self.stat.faults = 0;
                self.stat.last_error = None;
                self.stat.call_peak_us = self.stat.call_peak_us.max(elapsed_us);
                Ok(value.0)
            }
            Err(error) => {
                self.stat.faults += 1;
                let message = format!("{} 调用失败: {error}", self.export);
                self.stat.last_error = Some(message.clone());
                if self.stat.faults >= MAX_FAULTS {
                    self.stat.disabled = true;
                }
                Err(message)
            }
        }
    }

    pub fn stat(&self) -> &WasmStat {
        &self.stat
    }
}

#[cfg(test)]
#[path = "wasm_tests.rs"]
mod tests;
