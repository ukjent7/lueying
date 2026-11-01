//! 旁路进程层:本地大模型、高精度 OCR 这类重活的隔离运行位置。
//!
//! 扩展是独立可执行文件,宿主按需启动,一行一请求的 JSON 协议通信;
//! 不把未签名代码加载进输入法进程。两种生命周期:[`Bypass`] 一次调用
//! 一次进程,轻载场景用;[`NativeHost`] 常驻到空闲回收,模型等初始化
//! 成本只付一次。进程边界即信任边界:限额交 [`ProcessLimiter`] 钩子,
//! Windows 侧用 Job Object 实现,平台无关的测试用空实现;原生扩展在
//! Windows 上再加一层无能力的 AppContainer。
//!
//! 协议:请求是 `{"method":"...","args":...}\n`,应答是
//! `{"result":...}\n` 或 `{"error":"..."}\n`。进程退出码非零、行不是
//! 合法 JSON 或超时都算失败;超时的进程被宿主终止并停用语义由调用方
//! 记账(与脚本层的连续失败停用一致)。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lueying_extsvc::{Done, Provider};

pub use process::Process;

/// 一次旁路调用的配置。
#[derive(Clone, Debug)]
pub struct BypassConfig {
    /// 旁路可执行文件路径;宿主不校验签名,路径由用户配置与同意流程管。
    pub exe: PathBuf,
    /// 传给可执行文件的参数。
    pub args: Vec<String>,
    /// 单次调用的应答预算;含进程冷启动。
    pub call_timeout: Duration,
}

/// 平台限额钩子:进程刚启动、未通信前调用。
///
/// `Err` 表示限额装不上,调用方必须终止进程并把本次调用判为失败。
pub trait ProcessLimiter: Send + Sync {
    fn attach(&self, process: &Process) -> Result<(), String>;
}

/// 旁路进程客户端。克隆便宜;每次 [`Bypass::call`] 拉起新进程。
#[derive(Clone)]
pub struct Bypass {
    config: Arc<BypassConfig>,
    limiter: Option<Arc<dyn ProcessLimiter>>,
}

impl Bypass {
    pub fn new(config: BypassConfig) -> Self {
        Self {
            config: Arc::new(config),
            limiter: None,
        }
    }

    /// 装平台限额;Windows 宿主装 Job Object,无限额平台不装。
    pub fn with_limiter(mut self, limiter: Arc<dyn ProcessLimiter>) -> Self {
        self.limiter = Some(limiter);
        self
    }

    /// 冷启动 + 往返一次调用。同步阻塞;扩展侧的异步语义由
    /// [`FnProvider`] 在独立线程完成。
    pub fn call(&self, method: &str, args: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("旁路参数不是合法 JSON: {error}"))?;
        let request = serde_json::json!({ "method": method, "args": args }).to_string();
        let child = Command::new(&self.config.exe)
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("旁路进程启动失败: {error}"))?;
        let mut child = Process::from(child);
        if let Err(error) = self.attach(&mut child) {
            kill(&mut child);
            return Err(error);
        }
        let Some(stdout) = child.take_stdout() else {
            kill(&mut child);
            return Err("旁路进程没有标准输出".to_string());
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = {
            let spawn = std::thread::Builder::new()
                .name("bypass-read".to_string())
                .spawn(move || {
                    let _ = sender.send(read_response(stdout));
                });
            match spawn {
                Ok(reader) => reader,
                Err(error) => {
                    kill(&mut child);
                    return Err(format!("旁路读线程启动失败: {error}"));
                }
            }
        };
        let result = self.exchange(&mut child, &request, receiver, reader);
        kill(&mut child);
        result
    }

    fn attach(&self, child: &mut Process) -> Result<(), String> {
        match &self.limiter {
            Some(limiter) => limiter.attach(child),
            None => Ok(()),
        }
    }

    /// 写请求、按预算等应答;超时即杀,管道闭合后读线程随之结束。
    fn exchange(
        &self,
        child: &mut Process,
        request: &str,
        receiver: std::sync::mpsc::Receiver<Result<String, String>>,
        reader: std::thread::JoinHandle<()>,
    ) -> Result<String, String> {
        {
            let Some(mut stdin) = child.take_stdin() else {
                return Err("旁路进程没有标准输入".to_string());
            };
            // 请求行很小,管道缓冲装得下,写不会阻塞在没读的进程上。
            stdin
                .write_all(request.as_bytes())
                .and_then(|()| stdin.flush())
                .map_err(|error| format!("写旁路请求失败: {error}"))?;
        }
        // stdin 在块尾丢弃:写端 EOF,应答完进程可以自然退出。
        let line = match receiver.recv_timeout(self.config.call_timeout) {
            Ok(result) => result?,
            Err(_) => {
                // 终止进程让管道闭合,读线程随 EOF 收尾,不留悬挂线程。
                kill(child);
                let _ = reader.join();
                return Err(format!("旁路调用超时({:?})", self.config.call_timeout));
            }
        };
        // 读到应答只说明响应到达;进程还得自己体面退出,退出码也是
        // 协议的一部分(响应成功但退出码非零按失败处理)。
        reap(child, self.config.call_timeout)?;
        parse_response(&line)
    }
}

/// 读一行应答;EOF 与 IO 错误都归为失败。
fn read_response(stdout: Box<dyn Read + Send>) -> Result<String, String> {
    let mut line = String::new();
    let mut reader = BufReader::new(stdout);
    match reader.read_line(&mut line) {
        Ok(0) => Err("旁路进程无应答".to_string()),
        Ok(_) => Ok(line),
        Err(error) => Err(format!("读旁路应答失败: {error}")),
    }
}

/// 等进程退出;预算内不退出就终止。返回错误前必已回收进程。
fn reap(child: &mut Process, budget: Duration) -> Result<(), String> {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("旁路进程异常退出: {status}"))
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill(child);
                    return Err(format!("旁路进程未在预算内退出({budget:?})"));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(format!("等待旁路进程失败: {error}")),
        }
    }
}

/// 终止并回收;失败方向是进程被杀,错误不再上抛。
fn kill(child: &mut Process) {
    let _ = child.kill();
    let _ = child.wait();
}

/// 应答行的形状;`error` 优先,`result` 原样回传文本。
enum Reply {
    Result(String),
    Error(String),
    Malformed(String),
}

/// 把一行应答分成结果、业务错误与协议破坏。
fn classify_response(line: &str) -> Reply {
    let value = match serde_json::from_str::<serde_json::Value>(line.trim()) {
        Ok(value) => value,
        Err(error) => return Reply::Malformed(format!("旁路应答不是合法 JSON: {error}")),
    };
    if let Some(error) = value.get("error") {
        return Reply::Error(match error.as_str() {
            Some(text) => text.to_string(),
            None => error.to_string(),
        });
    }
    match value.get("result") {
        Some(result) => Reply::Result(result.to_string()),
        None => Reply::Malformed("旁路应答缺少 result 字段".to_string()),
    }
}

/// 一次性调用的应答只分成败;形状问题统一算失败。
fn parse_response(line: &str) -> Result<String, String> {
    match classify_response(line) {
        Reply::Result(result) => Ok(result),
        Reply::Error(error) => Err(error),
        Reply::Malformed(error) => Err(error),
    }
}

/// 把任意同步调用函数装进服务缝的提供者槽位:本地大模型、原生扩展等
/// 由独立进程承担,宿主进程内只留一个转发函数。
///
/// 每次调用在独立线程转发,不阻塞扩展宿主的泵任务;结果照原样过
/// [`Done`] 完成队列,隐私与审计在 [`Provider`] 之外的注册表层生效。
pub struct FnProvider<F> {
    call: Arc<F>,
}

impl<F> FnProvider<F>
where
    F: Fn(&str, &str) -> Result<String, String> + Send + Sync + 'static,
{
    pub fn new(call: F) -> Self {
        Self {
            call: Arc::new(call),
        }
    }
}

impl<F> Provider for FnProvider<F>
where
    F: Fn(&str, &str) -> Result<String, String> + Send + Sync + 'static,
{
    fn call(&self, _caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        let call = Arc::clone(&self.call);
        let method = method.to_string();
        let args = args.to_string();
        std::thread::Builder::new()
            .name("bypass-call".to_string())
            .spawn(move || done.complete(call(&method, &args)))
            .map_err(|error| format!("旁路调用线程启动失败: {error}"))?;
        Ok(())
    }
}

/// 常驻原生扩展的运行配置。
#[derive(Clone, Debug)]
pub struct NativeConfig {
    /// 原生可执行文件路径。
    pub exe: PathBuf,
    /// 传给可执行文件的参数。
    pub args: Vec<String>,
    /// 单次调用的应答预算。
    pub call_timeout: Duration,
    /// 空闲多久后收掉进程;进程再被调用会重新拉起。
    pub idle_exit: Duration,
    /// 额外授予 AppContainer 只读执行权限的路径(模型目录等)。
    /// Windows 之外的平台忽略。
    pub readable: Vec<PathBuf>,
}

struct Live {
    child: Process,
    stdin: Box<dyn Write + Send>,
    replies: mpsc::Receiver<Result<String, String>>,
    last_used: Instant,
}

impl Drop for Live {
    fn drop(&mut self) {
        kill(&mut self.child);
    }
}

struct Shared {
    config: NativeConfig,
    limiter: Option<Arc<dyn ProcessLimiter>>,
    live: Arc<Mutex<Option<Live>>>,
}

/// 常驻原生扩展宿主:首次调用拉起进程,之后进程跨调用存活,模型等
/// 初始化成本只付一次;空闲 [`NativeConfig::idle_exit`] 后由看护线程
/// 收走,保住零空闲成本。协议与 [`Bypass`] 相同(一行请求一行应答的
/// JSON),调用串行,同一时刻最多一次在途调用。
///
/// Windows 上进程运行在无能力的 AppContainer 里(派生包 SID,不授予
/// 任何能力),外加可选的 [`ProcessLimiter`] 限额;进程死亡后下一次
/// 调用自动重新拉起。
#[derive(Clone)]
pub struct NativeHost {
    shared: Arc<Shared>,
}

impl NativeHost {
    pub fn new(config: NativeConfig) -> Self {
        let host = Self {
            shared: Arc::new(Shared {
                config,
                limiter: None,
                live: Arc::new(Mutex::new(None)),
            }),
        };
        host.spawn_reaper();
        host
    }

    /// 装平台限额;Windows 宿主装 Job Object,无限额平台不装。
    pub fn with_limiter(mut self, limiter: Arc<dyn ProcessLimiter>) -> Self {
        Arc::get_mut(&mut self.shared)
            .expect("NativeHost 尚未共享")
            .limiter = Some(limiter);
        self
    }

    /// 冷启动(仅首次)+ 往返一次调用。同步阻塞;扩展侧的异步语义由
    /// [`FnProvider`] 在独立线程完成。
    pub fn call(&self, method: &str, args: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("原生扩展参数不是合法 JSON: {error}"))?;
        let request = serde_json::json!({ "method": method, "args": args }).to_string();
        let mut live = self
            .shared
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(running) = live.as_ref() {
            // 一问一答不该有残留;坏扩展多答的行在这里丢弃。
            while running.replies.try_recv().is_ok() {}
        }
        let dead = match live.as_mut() {
            None => true,
            Some(running) => running
                .child
                .try_wait()
                .map_or(true, |status| status.is_some()),
        };
        if dead {
            // 换成 None 先收走旧进程(Drop 兜底 kill),再拉新进程。
            *live = None;
            *live = Some(self.spawn()?);
        }
        let running = live.as_mut().expect("上一步刚拉起进程");
        if let Err(error) = write_request(&mut running.stdin, &request) {
            *live = None;
            return Err(error);
        }
        running.last_used = Instant::now();
        let budget = self.shared.config.call_timeout;
        match running.replies.recv_timeout(budget) {
            Ok(Ok(line)) => match classify_response(&line) {
                Reply::Result(result) => {
                    running.last_used = Instant::now();
                    Ok(result)
                }
                // 干净的业务错误:进程没坏,留着复用。
                Reply::Error(error) => Err(error),
                // 应答不成形说明进程已坏,收掉等下次重拉。
                Reply::Malformed(error) => {
                    *live = None;
                    Err(error)
                }
            },
            Ok(Err(error)) => {
                *live = None;
                Err(error)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                *live = None;
                Err(format!("原生扩展调用超时({budget:?})"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                *live = None;
                Err("原生扩展应答通道关闭".to_string())
            }
        }
    }

    fn spawn(&self) -> Result<Live, String> {
        let mut child = spawn::spawn_ext(
            &self.shared.config.exe,
            &self.shared.config.args,
            &self.shared.config.readable,
            self.shared.limiter.as_deref(),
        )?;
        let Some(stdin) = child.take_stdin() else {
            kill(&mut child);
            return Err("原生扩展没有标准输入".to_string());
        };
        let Some(stdout) = child.take_stdout() else {
            kill(&mut child);
            return Err("原生扩展没有标准输出".to_string());
        };
        let (sender, receiver) = mpsc::channel();
        let reader = match std::thread::Builder::new()
            .name("ext-native-read".to_string())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            let _ = sender.send(Err("原生扩展已退出".to_string()));
                            break;
                        }
                        Ok(_) => {
                            if sender.send(Ok(line)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = sender.send(Err(format!("读原生扩展应答失败: {error}")));
                            break;
                        }
                    }
                }
            }) {
            Ok(reader) => reader,
            Err(error) => {
                kill(&mut child);
                return Err(format!("原生扩展读线程启动失败: {error}"));
            }
        };
        // JoinHandle 在这里丢弃:读线程持有 stdout 自持到 EOF。
        let _ = reader;
        Ok(Live {
            child,
            stdin,
            replies: receiver,
            last_used: Instant::now(),
        })
    }

    /// 空闲看护:定期探测,超过预算没被调用就收走进程。探测间隔取
    /// 预算的四分之一,夹在 50 ms 与 15 s 之间;在途调用持有锁时跳过。
    fn spawn_reaper(&self) {
        let live = Arc::downgrade(&self.shared.live);
        let idle = self.shared.config.idle_exit;
        let step = (idle / 4).clamp(Duration::from_millis(50), Duration::from_secs(15));
        let _ = std::thread::Builder::new()
            .name("ext-native-idle".to_string())
            .spawn(move || {
                loop {
                    std::thread::sleep(step);
                    let Some(live) = live.upgrade() else {
                        break;
                    };
                    if let Ok(mut live) = live.try_lock()
                        && let Some(running) = live.as_ref()
                        && running.last_used.elapsed() >= idle
                    {
                        // Drop 收进程;宿主退出时 Job 的 kill-on-close 兜底。
                        *live = None;
                    }
                }
            });
    }
}

/// 写一行请求。常驻进程的 stdin 跨调用保持打开,写不关闭。
fn write_request(stdin: &mut (dyn Write + Send), request: &str) -> Result<(), String> {
    stdin
        .write_all(request.as_bytes())
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("写原生扩展请求失败: {error}"))
}

/// 给路径授 AppContainer 只读执行(文件或目录);非 Windows 平台是空操作。
/// 幂等;给每次调用的数据文件(截图)用,目录授权在拉起时已做。
#[cfg(windows)]
pub fn grant_appcontainer_read(path: &std::path::Path) -> Result<(), String> {
    sandbox::grant_read_execute(path)
}

#[cfg(not(windows))]
pub fn grant_appcontainer_read(_path: &std::path::Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub mod job;
mod process;
#[cfg(windows)]
pub(crate) mod sandbox;
mod spawn;
