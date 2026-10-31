//! 旁路进程层:本地大模型、高精度 OCR 这类重活的隔离运行位置。
//!
//! 扩展是独立可执行文件,宿主按需启动,一行一请求的 JSON 协议通信;
//! 不把未签名代码加载进输入法进程。默认不常驻:原型按「一次调用一次
//! 进程」实现,冷启动成本是测量项之一,常驻化等测量结果决定。进程边
//! 界即信任边界:限额交 [`ProcessLimiter`] 钩子,Windows 侧用 Job
//! Object 实现,平台无关的测试用空实现。
//!
//! 协议:请求是 `{"method":"...","args":...}\n`,应答是
//! `{"result":...}\n` 或 `{"error":"..."}\n`。进程退出码非零、行不是
//! 合法 JSON 或超时都算失败;超时的进程被宿主终止并停用语义由调用方
//! 记账(与脚本层的连续失败停用一致)。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lueying_extsvc::{Done, Provider};

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
    fn attach(&self, process: &Child) -> Result<(), String>;
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
    /// [`BypassProvider`] 在独立线程完成。
    pub fn call(&self, method: &str, args: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(args)
            .map_err(|error| format!("旁路参数不是合法 JSON: {error}"))?;
        let request = serde_json::json!({ "method": method, "args": args }).to_string();
        let mut child = Command::new(&self.config.exe)
            .args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("旁路进程启动失败: {error}"))?;
        if let Err(error) = self.attach(&mut child) {
            kill(&mut child);
            return Err(error);
        }
        let Some(stdout) = child.stdout.take() else {
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

    fn attach(&self, child: &mut Child) -> Result<(), String> {
        match &self.limiter {
            Some(limiter) => limiter.attach(child),
            None => Ok(()),
        }
    }

    /// 写请求、按预算等应答;超时即杀,管道闭合后读线程随之结束。
    fn exchange(
        &self,
        child: &mut Child,
        request: &str,
        receiver: std::sync::mpsc::Receiver<Result<String, String>>,
        reader: std::thread::JoinHandle<()>,
    ) -> Result<String, String> {
        {
            let Some(mut stdin) = child.stdin.take() else {
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
fn read_response(stdout: ChildStdout) -> Result<String, String> {
    let mut line = String::new();
    let mut reader = BufReader::new(stdout);
    match reader.read_line(&mut line) {
        Ok(0) => Err("旁路进程无应答".to_string()),
        Ok(_) => Ok(line),
        Err(error) => Err(format!("读旁路应答失败: {error}")),
    }
}

/// 等进程退出;预算内不退出就终止。返回错误前必已回收进程。
fn reap(child: &mut Child, budget: Duration) -> Result<(), String> {
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
fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// 应答行只认两种形状;`error` 优先,`result` 原样回传文本。
fn parse_response(line: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|error| format!("旁路应答不是合法 JSON: {error}"))?;
    if let Some(error) = value.get("error") {
        return Err(match error.as_str() {
            Some(text) => text.to_string(),
            None => error.to_string(),
        });
    }
    match value.get("result") {
        Some(result) => Ok(result.to_string()),
        None => Err("旁路应答缺少 result 字段".to_string()),
    }
}

/// 把旁路进程装进服务缝的提供者槽位:本地大模型、高精度 OCR 等由
/// 独立进程承担,宿主进程内只留一个 spawn 句柄。
///
/// 每次调用在独立线程转发,不阻塞扩展宿主的泵任务;结果照原样过
/// [`Done`] 完成队列,隐私与审计在 [`Provider`] 之外的注册表层生效。
pub struct BypassProvider {
    bypass: Bypass,
}

impl BypassProvider {
    pub fn new(bypass: Bypass) -> Self {
        Self { bypass }
    }
}

impl Provider for BypassProvider {
    fn call(&self, method: &str, args: &str, done: Done) -> Result<(), String> {
        let bypass = self.bypass.clone();
        let method = method.to_string();
        let args = args.to_string();
        std::thread::Builder::new()
            .name("bypass-call".to_string())
            .spawn(move || done.complete(bypass.call(&method, &args)))
            .map_err(|error| format!("旁路调用线程启动失败: {error}"))?;
        Ok(())
    }
}

#[cfg(windows)]
pub mod job;

#[cfg(test)]
pub(crate) fn test_echo_exe() -> std::path::PathBuf {
    // CARGO_BIN_EXE 对 lib 单测不一定注入;退而求其次,从测试二进制
    // 往上找同包的 bypass-echo(cargo test 总会把包内 bin 一起编出)。
    if let Some(path) = option_env!("CARGO_BIN_EXE_bypass-echo") {
        return std::path::PathBuf::from(path);
    }
    let bin = format!("bypass-echo{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.ancestors()
                .skip(1)
                .map(|dir| dir.join(&bin))
                .find(|candidate| candidate.is_file())
        })
        .expect("bypass-echo 应随测试一起编译")
}

#[cfg(test)]
#[path = "bypass_tests.rs"]
mod tests;
