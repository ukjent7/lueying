//! 原生扩展宿主:把独立的可执行程序当作常驻服务进程。
//!
//! 扩展是独立可执行文件,宿主按需启动,一行一请求的 JSON 协议通信;
//! 不把代码加载进调用方进程。不设沙箱与资源限额:扩展的风险由用户承担。
//!
//! 协议:请求是 `{"method":"...","args":...}\n`,应答是
//! `{"result":...}\n` 或 `{"error":"..."}\n`。进程退出、行不是合法 JSON
//! 或超时都算失败;超时的进程被宿主终止,下一次调用重新拉起。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

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

/// 强制收掉进程并等待回收;收尸失败没有补救手段,忽略错误。
fn kill(child: &mut Child) {
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
}

struct Live {
    child: Child,
    stdin: std::process::ChildStdin,
    replies: mpsc::Receiver<Result<String, String>>,
    last_used: Instant,
}

impl Drop for Live {
    fn drop(&mut self) {
        kill(&mut self.child);
    }
}

/// 常驻原生扩展宿主:首次调用拉起进程,之后进程跨调用存活,模型等
/// 初始化成本只付一次;空闲 [`NativeConfig::idle_exit`] 后由看护线程
/// 收走,保住零空闲成本。协议是一行请求一行应答的行 JSON,调用串行,
/// 同一时刻最多一次在途调用。进程死亡后下一次调用自动重新拉起。
#[derive(Clone)]
pub struct NativeHost {
    config: Arc<NativeConfig>,
    live: Arc<Mutex<Option<Live>>>,
}

impl NativeHost {
    /// 建宿主并启动空闲看护线程;此时还没拉起进程。
    pub fn new(config: NativeConfig) -> Self {
        let host = Self {
            config: Arc::new(config),
            live: Arc::new(Mutex::new(None)),
        };
        host.spawn_reaper();
        host
    }

    /// 冷启动(仅首次)+ 往返一次调用。同步阻塞,调用方在独立线程上等。
    pub fn call(&self, method: &str, args: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("原生扩展参数不是合法 JSON: {error}"))?;
        let request = serde_json::json!({ "method": method, "args": args }).to_string();
        let mut live = self
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
        let budget = self.config.call_timeout;
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
        let mut child = Command::new(&self.config.exe)
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("原生扩展启动失败: {error}"))?;
        let Some(stdin) = child.stdin.take() else {
            kill(&mut child);
            return Err("原生扩展没有标准输入".to_string());
        };
        let Some(stdout) = child.stdout.take() else {
            kill(&mut child);
            return Err("原生扩展没有标准输出".to_string());
        };
        let (sender, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new()
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
            });
        // JoinHandle 丢弃:读线程持有 stdout 自持到 EOF。
        if let Err(error) = spawned {
            kill(&mut child);
            return Err(format!("原生扩展读线程启动失败: {error}"));
        }
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
        let live = Arc::downgrade(&self.live);
        let idle = self.config.idle_exit;
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
                        // Drop 收进程。
                        *live = None;
                    }
                }
            });
    }
}

/// 写一行请求。常驻进程的 stdin 跨调用保持打开,行尾必须补换行,
/// 否则对端 `read_line` 会一直等到管道关闭。
fn write_request(stdin: &mut impl Write, request: &str) -> Result<(), String> {
    stdin
        .write_all(request.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("写原生扩展请求失败: {error}"))
}
