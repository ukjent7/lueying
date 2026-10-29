//! QuickJS 宿主:一个扩展包一个运行时,内存、栈、指令时间都有上限。
//!
//! 扩展跑在输入法进程里,任何一次派发都不能拖住击键。这里不追求通用 JS
//! 能力:没有事件循环、没有定时器、没有 I/O,只有 `lueying.register`、
//! `lueying.command` 与一次同步派发。超时用 QuickJS 的 interrupt handler
//! 打断,内存上限由运行时分配器兜底。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use rquickjs::{Context, Ctx, Function};
use serde::Deserialize;

use crate::SCENE_API;

const BOOTSTRAP: &str = include_str!("bootstrap.js");

/// 一次命令调用的结果:同步返回 JSON,或挂起等宿主泵作业。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invoke {
    /// 已完成;JSON 文本,空串表示没有返回值。
    Done(String),
    /// 返回了 Promise;等 [`Host::pump`] 兑现。
    Pending(u64),
}

/// 一条挂起任务的结算结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settled {
    /// 发起调用时拿到的任务号。
    pub token: u64,
    /// 是否成功;失败看 [`Settled::error`]。
    pub ok: bool,
    /// 成功时的 JSON 文本;`None` 表示 JS 返回 undefined。
    pub value: Option<String>,
    /// 失败时的错误文本。
    pub error: Option<String>,
}

/// 引导层回传的就绪结果。
#[derive(Deserialize)]
struct Ready {
    token: u64,
    ok: bool,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl From<Ready> for Settled {
    fn from(ready: Ready) -> Self {
        Self {
            token: ready.token,
            ok: ready.ok,
            value: ready.value,
            error: ready.error,
        }
    }
}

/// 单次泵作业的步数上限;被反复重新入队的任务留给下一轮泵。
const PUMP_STEPS: u32 = 256;

/// 运行期上限。
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// 运行时内存上限(字节)。
    pub memory_bytes: usize,
    /// JS 栈上限(字节)。
    pub stack_bytes: usize,
    /// 初始化(执行入口脚本)的时限(毫秒)。
    pub init_ms: u64,
    /// 单次派发的时限(毫秒)。
    pub dispatch_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 8 << 20,
            stack_bytes: 256 << 10,
            init_ms: 100,
            dispatch_ms: 8,
        }
    }
}

/// 一个扩展包的 JS 宿主。
pub struct Host {
    context: Context,
    epoch: Instant,
    deadline_ns: Arc<AtomicU64>,
    tripped: Arc<AtomicBool>,
    limits: Limits,
    /// 已挂起、尚未结算的任务数。
    pending: u64,
}

impl Host {
    /// 建运行时、装引导脚本、执行入口脚本。
    ///
    /// 入口脚本必须同步注册至少一个派发入口:`lueying.register(handler)`
    /// 或 `lueying.command(name, fn)`。顶层死循环由 [`Limits::init_ms`]
    /// 打断,语法错直接返回原文。
    pub fn new(source: &str, limits: Limits) -> Result<Self, String> {
        let runtime = rquickjs::Runtime::new().map_err(|e| format!("创建 JS 运行时失败: {e}"))?;
        runtime.set_memory_limit(limits.memory_bytes);
        runtime.set_max_stack_size(limits.stack_bytes);
        let epoch = Instant::now();
        let deadline_ns = Arc::new(AtomicU64::new(u64::MAX));
        let tripped = Arc::new(AtomicBool::new(false));
        {
            let deadline_ns = Arc::clone(&deadline_ns);
            let tripped = Arc::clone(&tripped);
            runtime.set_interrupt_handler(Some(Box::new(move || {
                let at = deadline_ns.load(Ordering::Relaxed);
                if at != u64::MAX && epoch.elapsed().as_nanos() as u64 >= at {
                    tripped.store(true, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            })));
        }
        let context = Context::full(&runtime).map_err(|e| format!("创建 JS 上下文失败: {e}"))?;
        let mut host = Self {
            context,
            epoch,
            deadline_ns,
            tripped,
            limits,
            pending: 0,
        };
        host.arm(limits.init_ms);
        let ready = host.context.with(|ctx| -> Result<bool, String> {
            let bootstrap = BOOTSTRAP.replace("__LUEYING_API__", &SCENE_API.to_string());
            ctx.eval::<(), _>(bootstrap)
                .map_err(|e| js_error(&ctx, e))?;
            ctx.eval::<(), _>(source).map_err(|e| js_error(&ctx, e))?;
            let ready: Function = ctx
                .globals()
                .get("__lueying_ready")
                .map_err(|e| js_error(&ctx, e))?;
            ready.call::<_, bool>(()).map_err(|e| js_error(&ctx, e))
        });
        host.disarm();
        let timed_out = host.tripped.swap(false, Ordering::Relaxed);
        match ready {
            Ok(true) => Ok(host),
            Ok(false) => Err("扩展没有注册 lueying.register 或 lueying.command".into()),
            Err(_) if timed_out => Err(format!("扩展初始化超时({} ms)", limits.init_ms)),
            Err(e) if is_oom(&e) => Err(format!(
                "扩展初始化超出内存上限({} MiB)",
                limits.memory_bytes >> 20
            )),
            Err(e) => Err(format!("扩展初始化失败: {e}")),
        }
    }

    /// 同步派发一次场景状态,返回脚本给的补丁 JSON(空串表示不改)。
    pub fn dispatch(&mut self, state_json: &str) -> Result<String, String> {
        let budget = self.limits.dispatch_ms;
        self.arm(budget);
        let result = self.context.with(|ctx| -> Result<String, String> {
            let dispatch: Function = ctx
                .globals()
                .get("__lueying_dispatch")
                .map_err(|e| js_error(&ctx, e))?;
            dispatch
                .call::<_, String>((state_json,))
                .map_err(|e| js_error(&ctx, e))
        });
        self.disarm();
        self.finish(result, "执行", budget)
    }

    /// 执行一条命令;参数与返回值都是 JSON 文本,空串表示无参数/无返回值。
    ///
    /// 命令名是扩展内的短名(清单里声明的那一个),不是全局完整 id;
    /// 完整 id 的解析与作用域由 [`crate::Runtime`] 负责。命令返回 Promise
    /// 时得到 [`Invoke::Pending`],由 [`Host::pump`] 推进。
    pub fn invoke(&mut self, command: &str, args_json: &str) -> Result<Invoke, String> {
        let budget = self.limits.dispatch_ms;
        self.arm(budget);
        let result = self.context.with(|ctx| -> Result<String, String> {
            let invoke: Function = ctx
                .globals()
                .get("__lueying_invoke")
                .map_err(|e| js_error(&ctx, e))?;
            invoke
                .call::<_, String>((command, args_json))
                .map_err(|e| js_error(&ctx, e))
        });
        self.disarm();
        let text = self.finish(result, "命令执行", budget)?;
        match text.strip_prefix("__pending:") {
            Some(token) => {
                let token = token
                    .parse::<u64>()
                    .map_err(|_| format!("坏的任务标记 {text:?}"))?;
                self.pending = self.pending.saturating_add(1);
                Ok(Invoke::Pending(token))
            }
            None => Ok(Invoke::Done(text)),
        }
    }

    /// 挂起、尚未结算的任务数。
    pub fn pending(&self) -> u64 {
        self.pending
    }

    /// 推进异步任务:按宿主时钟兑现到点定时器,跑一轮作业队列,收走结算结果。
    ///
    /// 每个作业步都受 [`Limits::dispatch_ms`] 约束;等待定时器与原生服务
    /// 不在预算内。一轮最多 [`PUMP_STEPS`] 步,反复重新入队的任务留给下一轮。
    pub fn pump(&mut self, now_ms: u64) -> Vec<Settled> {
        if self.pending == 0 {
            return Vec::new();
        }
        let budget = self.limits.dispatch_ms;
        let epoch = self.epoch;
        let deadline_ns = Arc::clone(&self.deadline_ns);
        let tripped = Arc::clone(&self.tripped);
        let mut pending = self.pending;
        let mut out = Vec::new();
        // 闭包只拿局部克隆,避免与 self.pending 的写冲突。
        self.context.with(|ctx| {
            if let Ok(tick) = ctx.globals().get::<_, Function>("__lueying_tick") {
                let _ = tick.call::<_, ()>((now_ms as f64,));
            }
            for _ in 0..PUMP_STEPS {
                tripped.store(false, Ordering::Relaxed);
                let nanos = u128::from(budget) * 1_000_000;
                let at = epoch
                    .elapsed()
                    .as_nanos()
                    .saturating_add(nanos)
                    .min(u128::from(u64::MAX)) as u64;
                deadline_ns.store(at, Ordering::Relaxed);
                let ran = ctx.execute_pending_job();
                deadline_ns.store(u64::MAX, Ordering::Relaxed);
                let killed = tripped.swap(false, Ordering::Relaxed);
                if !ran || killed {
                    break;
                }
            }
            if let Ok(take) = ctx.globals().get::<_, Function>("__lueying_take_ready") {
                if let Ok(json) = take.call::<_, String>(())
                    && let Ok(list) = serde_json::from_str::<Vec<Ready>>(&json)
                {
                    for ready in list {
                        pending = pending.saturating_sub(1);
                        out.push(ready.into());
                    }
                }
            }
        });
        self.pending = pending;
        out
    }

    /// 命令是否已注册;不跑用户代码,只查引导层里的表。
    pub fn has_command(&self, command: &str) -> bool {
        self.context.with(|ctx| {
            ctx.globals()
                .get::<_, Function>("__lueying_has_command")
                .ok()
                .and_then(|check| check.call::<_, bool>((command,)).ok())
                .unwrap_or(false)
        })
    }

    fn finish(
        &mut self,
        result: Result<String, String>,
        what: &str,
        budget: u64,
    ) -> Result<String, String> {
        let timed_out = self.tripped.swap(false, Ordering::Relaxed);
        match result {
            Ok(text) => Ok(text),
            Err(_) if timed_out => Err(format!("{what}超时({budget} ms)")),
            Err(error) if is_oom(&error) => Err(format!(
                "内存超出上限({} MiB)",
                self.limits.memory_bytes >> 20
            )),
            Err(error) => Err(error),
        }
    }

    fn arm(&mut self, ms: u64) {
        self.tripped.store(false, Ordering::Relaxed);
        let nanos = u128::from(ms) * 1_000_000;
        let at = self
            .epoch
            .elapsed()
            .as_nanos()
            .saturating_add(nanos)
            .min(u128::from(u64::MAX)) as u64;
        self.deadline_ns.store(at, Ordering::Relaxed);
    }

    fn disarm(&mut self) {
        self.deadline_ns.store(u64::MAX, Ordering::Relaxed);
    }
}

/// 把 QuickJS 异常转成一行可读文本;栈信息压成一行,避免日志刷屏。
fn js_error(ctx: &Ctx<'_>, err: rquickjs::Error) -> String {
    let value = ctx.catch();
    let text = if let Some(text) = value.as_string() {
        text.to_string().unwrap_or_default()
    } else if let Some(object) = value.as_object() {
        let name: String = object.get("name").unwrap_or_default();
        let message: String = object.get("message").unwrap_or_default();
        match (name.is_empty(), message.is_empty()) {
            (false, false) => format!("{name}: {message}"),
            (false, true) => name,
            (true, false) => message,
            (true, true) => String::new(),
        }
    } else {
        String::new()
    };
    let text = if text.is_empty() {
        err.to_string()
    } else {
        text
    };
    one_line(&text, 300)
}

/// 压成一行并截断;中文按字符数算,不切坏 UTF-8。
fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// QuickJS 的内存不足在不同版本里文本不同,统一认几种写法。
fn is_oom(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("out of memory")
        || lower.contains("could not allocate")
        || text.contains("内存不足")
}
