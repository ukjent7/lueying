//! 服务端侧的扩展宿主连接:拉起 `lueying-ext-host`,经标准输入输出交换行协议。
//!
//! 调用方永远不等待宿主:外观、上下文、候选快照都只保留最新值,命令与配置进有序队列,
//! 写与读各在自己的线程上跑。宿主卡死或崩溃时,补丁不再到达,超时的补丁一律丢弃,
//! 候选窗照常工作;监督线程发现宿主退出后重拉,并重发当前配置。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lueying_ext::proto::candidate::CandidateState;
use lueying_ext::proto::link::{Context, ToHost, ToServer, decode, encode};
use lueying_ext::proto::{SceneIn, ScenePatch};

/// 外观补丁的有效期:超过它还没回来的补丁对应的画面已经过时,直接丢弃。
pub const PATCH_TIMEOUT: Duration = Duration::from_secs(1);
/// 重拉宿主前的等待,避免宿主反复起不来时打满 CPU。
const RESTART: Duration = Duration::from_millis(500);

/// 宿主请求的一次服务调用。
pub struct Request {
    /// 发起调用的扩展 id。
    pub caller: String,
    /// 服务名:`output`、`panel` 或 `context`。
    pub service: String,
    /// 方法名。
    pub method: String,
    /// 方法参数 JSON。
    pub args: String,
}

/// 回填一次请求的结果;消耗它即回送 [`ToHost::Reply`]。
pub struct Replier {
    shared: Arc<Shared>,
    id: u64,
}

impl Replier {
    /// 回送结果。
    pub fn send(self, result: Result<String, String>) {
        self.shared.push_ctrl(ToHost::Reply {
            id: self.id,
            result,
        });
    }
}

/// 请求处理函数:在服务端线程上调用,结果经 [`Replier`] 异步回送。
pub type Handler = Arc<dyn Fn(u64, Request, Replier) + Send + Sync>;

/// 宿主连接句柄。克隆便宜。
pub struct ExtLink {
    shared: Arc<Shared>,
    wake: SyncSender<()>,
}

/// 线程之间共享的状态。
struct Shared {
    queue: Mutex<Queue>,
    /// 最近一次采纳的补丁:序号与内容。
    patch: Mutex<Option<(u64, Option<ScenePatch>)>>,
    /// 最近一次提交的外观序号与提交时刻,用来判断补丁是否超时。
    submitted: Mutex<(u64, Instant)>,
    titles: Mutex<Vec<(String, String)>>,
    /// 补丁到达时调用:服务端据此叫醒候选窗重绘。
    on_patch: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    handler: Handler,
}

impl Shared {
    fn push_ctrl(&self, message: ToHost) {
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ctrl
            .push_back(message);
    }
}

/// 待写出的消息。最新值槽只保留最后一条;配置有代数,重拉宿主后会整份重发。
#[derive(Default)]
struct Queue {
    ctrl: VecDeque<ToHost>,
    config: Option<(u64, ToHost)>,
    config_gen: u64,
    context: Option<ToHost>,
    candidates: Option<ToHost>,
    scene: Option<ToHost>,
}

impl ExtLink {
    /// 拉起宿主并启动监督线程。`args` 是宿主的本机目录参数;宿主的诊断写进 `log`(没有则丢弃);
    /// `handler` 处理宿主的服务请求。
    pub fn spawn(exe: PathBuf, args: Vec<String>, log: Option<PathBuf>, handler: Handler) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            patch: Mutex::new(None),
            submitted: Mutex::new((0, Instant::now())),
            titles: Mutex::new(Vec::new()),
            on_patch: Mutex::new(None),
            handler,
        });
        let (wake, wake_rx) = mpsc::sync_channel(1);
        let supervisor_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("ext-host-link".into())
            .spawn(move || supervise(&supervisor_shared, &exe, &args, log.as_deref(), &wake_rx));
        if let Err(error) = spawned {
            println!("扩展宿主连接线程启动失败: {error}");
        }
        Self { shared, wake }
    }

    fn kick(&self) {
        let _ = self.wake.try_send(());
    }

    /// 装补丁到达回调。
    pub fn on_patch(&self, callback: impl Fn() + Send + Sync + 'static) {
        *self
            .shared
            .on_patch
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(callback));
    }

    /// 设置生效配置。宿主重拉后会自动重发。
    pub fn set_config(
        &self,
        extensions: Vec<String>,
        bindings: Vec<(String, String)>,
        ppocr: bool,
    ) {
        {
            let mut queue = self.shared.queue.lock().unwrap_or_else(|p| p.into_inner());
            queue.config_gen += 1;
            let generation = queue.config_gen;
            queue.config = Some((
                generation,
                ToHost::Config {
                    extensions,
                    bindings,
                    ppocr,
                },
            ));
        }
        self.kick();
    }

    /// 输入上下文变了:只保留最新一份。
    pub fn set_context(&self, context: Context) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .context = Some(ToHost::Context(context));
        self.kick();
    }

    /// 候选窗快照:只保留最新一份,转给订阅了候选窗事件的扩展。
    pub fn publish_candidates(&self, state: CandidateState) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .candidates = Some(ToHost::Candidates { state });
        self.kick();
    }

    /// 提交一帧外观状态,返回它的序号;宿主忙时只保留最后一帧。
    pub fn submit_scene(&self, scene: SceneIn) -> u64 {
        let seq = {
            let mut submitted = self
                .shared
                .submitted
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            submitted.0 += 1;
            submitted.1 = Instant::now();
            submitted.0
        };
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .scene = Some(ToHost::Scene { seq, scene });
        self.kick();
        seq
    }

    /// 最近一次提交的外观序号。
    pub fn scene_seq(&self) -> u64 {
        self.shared
            .submitted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .0
    }

    /// 取走最新补丁;没有新补丁返回 `None`。补丁在到达时已按序号与超时过滤。
    pub fn take_patch(&self) -> Option<(u64, Option<ScenePatch>)> {
        self.shared
            .patch
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    /// 动作候选与外部启动器用的命令标题表(宿主上报的最新版本)。
    pub fn titles(&self) -> Vec<(String, String)> {
        self.shared
            .titles
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// 投递一条外部触发的命令;宿主不在线时消息留在队列里。
    pub fn invoke(&self, command: &str, source: &str) {
        self.shared.push_ctrl(ToHost::Invoke {
            command: command.to_string(),
            source: source.to_string(),
        });
        self.kick();
    }
}

/// 监督线程:拉起宿主,读写各自跑;宿主退出就重拉。
fn supervise(
    shared: &Arc<Shared>,
    exe: &PathBuf,
    args: &[String],
    log: Option<&std::path::Path>,
    wake: &Receiver<()>,
) {
    loop {
        let stderr = log
            .and_then(|path| {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            })
            .map_or_else(Stdio::null, Stdio::from);
        let mut command = Command::new(exe);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        let mut child: Child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                println!("扩展宿主启动失败: {error}");
                std::thread::sleep(RESTART);
                continue;
            }
        };
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            std::thread::sleep(RESTART);
            continue;
        };
        let reader_shared = Arc::clone(shared);
        let reader = std::thread::Builder::new()
            .name("ext-host-read".into())
            .spawn(move || read_loop(stdout, &reader_shared));
        write_loop(stdin, shared, wake);
        let _ = child.kill();
        let _ = child.wait();
        if let Ok(reader) = reader {
            let _ = reader.join();
        }
        println!(
            "扩展宿主退出,{} 秒后重拉",
            RESTART.as_millis() as f64 / 1000.0
        );
        std::thread::sleep(RESTART);
    }
}

/// 写线程:每次唤醒把队列里的消息按顺序写出;写失败即返回,交给监督线程重拉。
fn write_loop(mut stdin: std::process::ChildStdin, shared: &Shared, wake: &Receiver<()>) {
    let mut sent_config = 0;
    loop {
        match wake.recv_timeout(Duration::from_millis(250)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let batch = {
            let mut queue = shared.queue.lock().unwrap_or_else(|p| p.into_inner());
            let mut batch = Vec::new();
            if let Some((generation, message)) = &queue.config
                && *generation > sent_config
            {
                sent_config = *generation;
                batch.push(message.clone());
            }
            batch.extend(queue.ctrl.drain(..));
            batch.extend(queue.context.take());
            batch.extend(queue.candidates.take());
            batch.extend(queue.scene.take());
            batch
        };
        for message in batch {
            let Ok(line) = encode(&message) else {
                continue;
            };
            if writeln!(stdin, "{line}")
                .and_then(|()| stdin.flush())
                .is_err()
            {
                return;
            }
        }
    }
}

/// 读线程:逐行解码宿主消息。补丁与标题直接入槽;服务请求交给处理函数。
fn read_loop(stdout: std::process::ChildStdout, shared: &Arc<Shared>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            return;
        };
        match decode::<ToServer>(&line) {
            Ok(ToServer::Patch { seq, patch }) => {
                // 只采纳当前序号、且在超时窗口内回来的补丁;其余是过期结果,丢弃。
                let fresh = {
                    let submitted = shared.submitted.lock().unwrap_or_else(|p| p.into_inner());
                    seq == submitted.0 && submitted.1.elapsed() <= PATCH_TIMEOUT
                };
                if !fresh {
                    continue;
                }
                *shared.patch.lock().unwrap_or_else(|p| p.into_inner()) = Some((seq, patch));
                let callback = shared
                    .on_patch
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                if let Some(callback) = callback {
                    callback();
                }
            }
            Ok(ToServer::Titles { commands }) => {
                *shared.titles.lock().unwrap_or_else(|p| p.into_inner()) = commands;
            }
            Ok(ToServer::Request {
                id,
                caller,
                service,
                method,
                args,
            }) => {
                let replier = Replier {
                    shared: Arc::clone(shared),
                    id,
                };
                (shared.handler)(
                    id,
                    Request {
                        caller,
                        service,
                        method,
                        args,
                    },
                    replier,
                );
            }
            Err(error) => println!("扩展宿主消息坏行: {error}"),
        }
    }
}
