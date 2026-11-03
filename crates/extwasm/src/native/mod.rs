//! 原生扩展宿主:把独立的可执行程序当作常驻服务进程。
//!
//! 扩展是独立可执行文件,宿主按需启动,一行一请求的 JSON 协议通信;
//! 不把未签名代码加载进输入法进程。进程边界即信任边界:限额交
//! [`ProcessLimiter`] 钩子,Windows 侧用 Job Object 实现,平台无关的
//! 测试用空实现;原生扩展在 Windows 上再加一层无能力的 AppContainer。
//!
//! 协议:请求是 `{"method":"...","args":...}\n`,应答是
//! `{"result":...}\n` 或 `{"error":"..."}\n`。进程退出码非零、行不是
//! 合法 JSON 或超时都算失败;超时的进程被宿主终止并停用语义由调用方
//! 记账(与脚本层的连续失败停用一致)。

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub use process::Process;

/// 平台限额钩子:进程刚启动、未通信前调用。
///
/// `Err` 表示限额装不上,调用方必须终止进程并把本次调用判为失败。
pub trait ProcessLimiter: Send + Sync {
    /// 给刚启动的进程装上限额;`Err` 时调用方会终止进程。
    fn attach(&self, process: &Process) -> Result<(), String>;
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
        Err(error) => return Reply::Malformed(format!("原生扩展应答不是合法 JSON: {error}")),
    };
    if let Some(error) = value.get("error") {
        return Reply::Error(match error.as_str() {
            Some(text) => text.to_string(),
            None => error.to_string(),
        });
    }
    match value.get("result") {
        Some(result) => Reply::Result(result.to_string()),
        None => Reply::Malformed("原生扩展应答缺少 result 字段".to_string()),
    }
}

/// 强制收掉进程并等待句柄回收;收尸失败没有补救手段,忽略错误。
fn kill(child: &mut Process) {
    let _ = child.kill();
    let _ = child.wait();
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
    /// 是否套沙箱(Windows 的无能力 AppContainer)。下载来的原生扩展要
    /// 沙箱;自有组件(联网 broker)要开网络,关掉。
    pub sandboxed: bool,
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
/// 收走,保住零空闲成本。协议是一行请求一行应答的行 JSON,调用串行,
/// 同一时刻最多一次在途调用。
///
/// Windows 上进程运行在无能力的 AppContainer 里(派生包 SID,不授予
/// 任何能力),外加可选的 [`ProcessLimiter`] 限额;进程死亡后下一次
/// 调用自动重新拉起。
#[derive(Clone)]
pub struct NativeHost {
    shared: Arc<Shared>,
}

impl NativeHost {
    /// 建宿主并启动空闲看护线程;此时还没拉起进程。
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

    /// 冷启动(仅首次)+ 往返一次调用。同步阻塞,调用方在独立线程上等。
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
            Some(running) => !matches!(running.child.try_wait(), Ok(None)),
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
            self.shared.config.sandboxed,
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

/// 写一行请求。常驻进程的 stdin 跨调用保持打开,行尾必须补换行,
/// 否则对端 `read_line` 会一直等到管道关闭。
fn write_request(stdin: &mut (dyn Write + Send), request: &str) -> Result<(), String> {
    stdin
        .write_all(request.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("写原生扩展请求失败: {error}"))
}

/// 给路径授 AppContainer 只读执行(文件或目录);非 Windows 平台是空操作。
/// 幂等;给每次调用的数据文件(截图)用,目录授权在拉起时已做。
#[cfg(windows)]
pub fn grant_appcontainer_read(path: &Path) -> Result<(), String> {
    sandbox::grant_read_execute(path)
}

#[cfg(not(windows))]
pub fn grant_appcontainer_read(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub mod job;
mod process;
#[cfg(windows)]
pub(crate) mod sandbox;
mod spawn;
