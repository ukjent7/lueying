//! 扩展宿主主循环:收服务端消息,换扩展、派发外观、执行命令与热键,回补丁与标题。
//!
//! 只有这一个线程碰 [`Runtime`];每个扩展自己的 worker 线程跑 Wasm。服务端从不等这里:
//! 外观补丁带序号回传,服务端只采用当前序号的补丁,超时或晚到的一律丢弃。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lueying_ext::host::bindings::BindingTable;
#[cfg(windows)]
use lueying_ext::host::bindings::Input;
use lueying_ext::host::{
    ConsentBook, ConsentGate, ExtMetrics, ExtStat, ExtState, METRICS_VERSION, PrivacyCheck,
    Registry, Runtime, SceneIn, ScenePatch, Service,
};
use lueying_ext::proto::link::{Context, ToHost, ToServer};

use crate::candidates;
use crate::channel::Outbound;
use crate::remote::{Grants, Pending, Remote};
#[cfg(windows)]
use crate::services::{self, HotkeyProvider};
use crate::state::{self, Paths};

/// 轮询目录与指标的间隔;改完脚本一秒内生效。
const POLL: Duration = Duration::from_millis(1000);
/// 同意记录与配置类变化的检查间隔。
const CONSENT: Duration = Duration::from_millis(500);
/// 指标快照的写入间隔;内容不变时不重写。
const METRICS: Duration = Duration::from_secs(1);
/// 指标内容长时间不变时也重写一次,让 `top` 看到新鲜时间。
const METRICS_HEARTBEAT: Duration = Duration::from_secs(30);

/// 输入上下文与隐私标记;服务调用、热键与派发都按它把关。
#[derive(Default)]
pub struct Shared {
    private: AtomicBool,
    editable: AtomicBool,
    composing: AtomicBool,
}

impl Shared {
    fn set(&self, context: Context) {
        self.private.store(context.private, Ordering::Relaxed);
        self.editable.store(context.editable, Ordering::Relaxed);
        self.composing.store(context.composing, Ordering::Relaxed);
    }

    #[cfg(windows)]
    fn input(&self) -> Input {
        Input {
            editable: self.editable.load(Ordering::Relaxed),
            composing: self.composing.load(Ordering::Relaxed),
        }
    }
}

/// 服务端下发的生效配置。
#[derive(Default)]
struct Config {
    order: Vec<String>,
    allow_all_network: bool,
    bindings: Vec<(String, String)>,
}

/// 运行一个扩展宿主直到输入关闭。返回进程退出码。
pub fn run(inbox: Receiver<ToHost>, out: Outbound, pending: Pending) -> i32 {
    let shared = Arc::new(Shared::default());
    let grants: Grants = Arc::new(Mutex::new(HashMap::new()));
    let privacy: PrivacyCheck = {
        let shared = Arc::clone(&shared);
        Arc::new(move || shared.private.load(Ordering::Relaxed))
    };
    let mut host = Host {
        out,
        pending,
        shared,
        #[cfg(windows)]
        hotkey: Arc::new(HotkeyProvider::default()),
        grants,
        privacy,
        config: Config::default(),
        runtime: None,
        skipped: Vec::new(),
        bindings: BindingTable::default(),
        bound: Vec::new(),
        titles: Vec::new(),
        reported: None,
        consent_stamp: consent_fingerprint(),
        next_request: Arc::new(AtomicU64::new(0)),
        scene: None,
        last_seq: 0,
        last_metrics: None,
        last_metrics_write: Instant::now(),
        metrics_failed: false,
    };
    candidates::spawn_subscribers();
    let mut next_poll = Instant::now();
    let mut next_consent = Instant::now();
    let mut next_metrics = Instant::now();
    loop {
        match inbox.recv_timeout(Duration::from_millis(20)) {
            Ok(message) => host.handle(message),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return 0,
        }
        while let Ok(message) = inbox.try_recv() {
            host.handle(message);
        }
        let now = Instant::now();
        if now >= next_consent {
            next_consent = now + CONSENT;
            let stamp = consent_fingerprint();
            if stamp != host.consent_stamp {
                host.consent_stamp = stamp;
                host.reload();
            }
        }
        if now >= next_poll {
            next_poll = now + POLL;
            host.poll();
        }
        if now >= next_metrics {
            next_metrics = now + METRICS;
            host.publish_metrics();
        }
        host.hotkeys();
        host.dispatch_scene();
    }
}

struct Host {
    out: Outbound,
    pending: Pending,
    shared: Arc<Shared>,
    /// 全局热键只在 Windows 上有;注册与 WM_HOTKEY 收取都在本线程。
    #[cfg(windows)]
    hotkey: Arc<HotkeyProvider>,
    grants: Grants,
    privacy: PrivacyCheck,
    config: Config,
    runtime: Option<Runtime>,
    /// 配置里启用但没进运行期的包(待同意、坏包);指标里也要列出来。
    skipped: Vec<ExtStat>,
    bindings: BindingTable,
    /// 当前绑定行的快照,变化才重绑。
    bound: Vec<(String, String)>,
    /// 上次发给服务端的标题表;变化才再发。
    titles: Vec<(String, String)>,
    reported: Option<String>,
    consent_stamp: Option<u64>,
    next_request: Arc<AtomicU64>,
    /// 最新外观状态;宿主忙时只保留最后一帧。
    scene: Option<(u64, SceneIn)>,
    last_seq: u64,
    last_metrics: Option<String>,
    last_metrics_write: Instant,
    metrics_failed: bool,
}

impl Host {
    fn handle(&mut self, message: ToHost) {
        match message {
            ToHost::Config {
                extensions,
                allow_all_network,
                bindings,
                ppocr,
            } => {
                state::set_ppocr(ppocr);
                let changed = extensions != self.config.order
                    || allow_all_network != self.config.allow_all_network;
                self.config = Config {
                    order: extensions,
                    allow_all_network,
                    bindings,
                };
                if changed {
                    self.reload();
                } else {
                    self.rebind();
                }
            }
            ToHost::Scene { seq, scene } => self.scene = Some((seq, scene)),
            ToHost::Context(context) => self.shared.set(context),
            ToHost::Candidates { state } => candidates::publish(state),
            ToHost::Invoke { command, source } => self.run_command(&command, &source),
            // 应答由读线程直接回填给等待的调用,不经过这里。
            ToHost::Reply { .. } => {}
        }
    }

    /// 按当前配置重建运行期:扩展、服务缝、绑定与标题。
    fn reload(&mut self) {
        #[cfg(windows)]
        self.hotkey.reset();
        let (runtime, skipped) = self.load();
        self.runtime = runtime;
        self.skipped = skipped;
        self.reported = None;
        self.last_metrics = None;
        // 换扩展后旧外观作废:先清空效果,等新扩展的补丁。
        self.out.send(&ToServer::Patch {
            seq: self.last_seq,
            patch: None,
        });
        self.bound.clear();
        self.rebind();
        self.send_titles();
    }

    fn rebind(&mut self) {
        let want = self.config.bindings.clone();
        if want == self.bound {
            return;
        }
        self.bound = want;
        self.bindings = self.bind_rows();
    }

    /// 配置绑定行 → 绑定表并注册热键;命令不存在或组合不合法的行写日志跳过。
    fn bind_rows(&self) -> BindingTable {
        let mut table = BindingTable::default();
        #[cfg(windows)]
        self.hotkey.unbind_users();
        if self.config.bindings.is_empty() {
            return table;
        }
        let Some(runtime) = self.runtime.as_ref() else {
            eprintln!("扩展未启用,{} 条绑定忽略", self.config.bindings.len());
            return table;
        };
        for (combo, command) in &self.config.bindings {
            let Some(info) = runtime.command(command) else {
                eprintln!("绑定 {combo} 跳过:没有命令 {command}");
                continue;
            };
            if let Err(error) = table.insert(combo, command.clone(), info.when) {
                eprintln!("绑定 {combo} 跳过:{error}");
                continue;
            }
            let row = table.rows().len() - 1;
            #[cfg(windows)]
            match self.hotkey.bind(row, combo) {
                Ok(()) => eprintln!("绑定 {combo} → {command}"),
                Err(error) => eprintln!("绑定 {combo} 注册失败:{error}"),
            }
            #[cfg(not(windows))]
            let _ = row;
        }
        table
    }

    /// 收热键触发:扩展 press 已在 pump 里就地兑现;用户绑定行按 when 过滤后执行。
    /// 隐私字段下热键直接丢弃,挂起的 press 收到拒绝。
    #[cfg(windows)]
    fn hotkeys(&mut self) {
        let private = (self.privacy)();
        if private {
            self.hotkey.cancel_presses("隐私字段下已停用扩展热键");
        }
        let fired = self.hotkey.pump();
        if private {
            if !fired.is_empty() {
                eprintln!("热键跳过:当前是密码/隐私字段");
            }
            return;
        }
        let input = self.shared.input();
        for row in fired {
            let Some(binding) = self.bindings.rows().get(row).cloned() else {
                continue;
            };
            if !input.allows(binding.when) {
                eprintln!("热键 {} 跳过:当前输入状态不满足 when", binding.hotkey);
                continue;
            }
            self.run_command(&binding.command, &format!("热键 {}", binding.hotkey));
        }
    }

    #[cfg(not(windows))]
    fn hotkeys(&mut self) {}

    /// 执行一条扩展命令;结果与错误由循环节拍写日志。
    fn run_command(&mut self, command: &str, source: &str) {
        if (self.privacy)() {
            eprintln!("{source} 跳过 {command}:当前是密码/隐私字段");
            return;
        }
        let Some(runtime) = self.runtime.as_mut() else {
            eprintln!("{source} 跳过:扩展未启用({command})");
            return;
        };
        match runtime.invoke(command, "{}") {
            Ok(seq) => eprintln!("{source} 触发 {command}(序号 {seq})"),
            Err(error) => eprintln!("{source} 执行失败:{error}"),
        }
    }

    /// 派发最新外观状态,补丁带序号回传。
    fn dispatch_scene(&mut self) {
        let Some((seq, scene)) = self.scene.take() else {
            return;
        };
        self.last_seq = seq;
        let patch: Option<ScenePatch> = self
            .runtime
            .as_mut()
            .and_then(|runtime| runtime.dispatch(&scene));
        self.out.send(&ToServer::Patch { seq, patch });
    }

    /// 轮询目录热重载与空闲卸载;运行期出错时只报一次。
    fn poll(&mut self) {
        let Some(current) = self.runtime.as_mut() else {
            return;
        };
        let removed = current.poll();
        current.idle_unload();
        let mut errors = Vec::new();
        for id in &self.config.order {
            if let Some(error) = current.error(id) {
                errors.push(format!("{id}: {error}"));
            }
        }
        if removed > 0 {
            self.out.send(&ToServer::Patch {
                seq: self.last_seq,
                patch: None,
            });
            self.send_titles();
        }
        for message in errors {
            if self.reported.as_deref() != Some(&message) {
                eprintln!("扩展运行失败: {message}");
                self.reported = Some(message);
            }
        }
    }

    /// 标题表变了才发:服务端据此判断触发词对应的命令是否可执行。
    fn send_titles(&mut self) {
        let mut titles: Vec<(String, String)> = self
            .runtime
            .as_ref()
            .map(|runtime| {
                runtime
                    .commands()
                    .into_iter()
                    .map(|info| (info.id, info.title))
                    .collect()
            })
            .unwrap_or_default();
        titles.sort();
        if titles != self.titles {
            self.out.send(&ToServer::Titles {
                commands: titles.clone(),
            });
            self.titles = titles;
        }
    }

    /// 按配置顺序打开扩展;坏包只记日志,不影响其他扩展。只加载已同意的包。
    fn load(&mut self) -> (Option<Runtime>, Vec<ExtStat>) {
        let mut skipped = Vec::new();
        let order = self.config.order.clone();
        if order.is_empty() {
            return (None, skipped);
        }
        let paths: &Paths = state::paths();
        let root = paths.extensions();
        let consent_path = paths.consent();
        let book = ConsentBook::load(&consent_path);
        let mut approved = Vec::new();
        let mut network_grants: Vec<(String, Vec<String>)> = Vec::new();
        let mut capability_grants: Vec<(String, Vec<String>)> = Vec::new();
        for id in &order {
            let dir = root.join(id);
            let manifest = match lueying_ext::host::manifest_at(&dir) {
                Ok(manifest) => manifest,
                Err(error) => {
                    eprintln!("扩展 {id} 未加载: {error}");
                    skipped.push(error_stat(id, error));
                    continue;
                }
            };
            if manifest.is_native() {
                // 原生扩展不进 Wasm 运行时:服务提供者按需拉起进程。
                eprintln!("原生扩展 {id}:由服务提供者按需调用");
                continue;
            }
            match book.check(&dir, &manifest) {
                Ok(check) if !check.required || check.approved => {
                    if let Some(grant) = &manifest.network {
                        network_grants.push((id.clone(), grant.allow.clone()));
                    }
                    if !manifest.capabilities.is_empty() {
                        capability_grants.push((id.clone(), manifest.capabilities.clone()));
                    }
                    approved.push(id.clone());
                }
                Ok(check) => {
                    eprintln!("扩展 {id} 待同意({})", check.items.join("、"));
                    skipped.push(base_stat(&manifest, ExtState::Unapproved, None));
                }
                Err(error) => {
                    eprintln!("扩展 {id} 同意检查失败: {error}");
                    skipped.push(base_stat(&manifest, ExtState::Error, Some(error)));
                }
            }
        }
        *self.grants.lock().unwrap_or_else(|p| p.into_inner()) =
            capability_grants.into_iter().collect();
        if approved.is_empty() {
            eprintln!("运行期扩展:没有已同意的包");
            return (None, skipped);
        }
        let registry = Arc::new(Registry::new());
        registry.set_privacy(Arc::clone(&self.privacy));
        self.install_services(&registry, network_grants);
        let gate: ConsentGate = {
            let path = consent_path;
            Arc::new(move |manifest, dir| {
                let book = ConsentBook::load(&path);
                let check = book
                    .check(dir, manifest)
                    .map_err(|error| format!("同意检查失败: {error}"))?;
                if check.required && !check.approved {
                    return Err(format!("需在设置里同意: {}", check.items.join("、")));
                }
                Ok(())
            })
        };
        let (runtime, errors) = Runtime::open_with_gate(&root, &approved, registry, Some(gate));
        for error in errors {
            eprintln!("{error}");
        }
        eprintln!("运行期扩展:{} 个槽位(已过同意)", approved.len());
        (Some(runtime), skipped)
    }

    /// 装服务缝:本进程的提供者直接装;输出、选择面板、上下文转给服务端。
    fn install_services(&self, registry: &Registry, network_grants: Vec<(String, Vec<String>)>) {
        for (service, name, grants) in [
            (Service::Output, "output", None),
            (Service::Panel, "panel", None),
            (Service::Context, "context", Some(Arc::clone(&self.grants))),
        ] {
            registry.set(
                service,
                Arc::new(Remote::new(
                    name,
                    self.out.clone(),
                    Arc::clone(&self.pending),
                    Arc::clone(&self.next_request),
                    grants,
                )),
            );
        }
        #[cfg(windows)]
        self.install_local(registry, network_grants);
        #[cfg(not(windows))]
        let _ = network_grants;
    }

    /// Windows 本地提供者:截图、识别、剪贴板、存储、音频、热键与联网。
    #[cfg(windows)]
    fn install_local(&self, registry: &Registry, network_grants: Vec<(String, Vec<String>)>) {
        registry.set(Service::Capture, Arc::new(services::CaptureProvider));
        registry.set(Service::Ocr, Arc::new(services::OcrProvider::new()));
        registry.set(
            Service::Clipboard,
            Arc::new(services::ClipboardProvider::new()),
        );
        registry.set(Service::Store, Arc::new(services::StoreProvider));
        registry.set(Service::Notify, Arc::new(services::NotifyProvider));
        registry.set(Service::Audio, Arc::new(services::AudioProvider));
        registry.set(Service::Asr, Arc::new(services::AsrProvider::new()));
        // 热键提供者由本线程持有:注册与 WM_HOTKEY 收取都在这里。
        registry.set(
            Service::Hotkey,
            Arc::clone(&self.hotkey) as Arc<dyn lueying_ext::host::Provider>,
        );
        let network = services::NetworkProvider::new();
        network.set_allow(network_grants);
        network.set_open_all(self.config.allow_all_network);
        registry.set(Service::Network, Arc::new(network));
    }

    /// 指标快照:内容没变时只在心跳到期后重写。
    fn publish_metrics(&mut self) {
        let mut extensions = self
            .runtime
            .as_ref()
            .map(Runtime::stats)
            .unwrap_or_default();
        extensions.extend_from_slice(&self.skipped);
        extensions.sort_by(|a, b| a.id.cmp(&b.id));
        let content = serde_json::to_string(&extensions).unwrap_or_default();
        let heartbeat = self.last_metrics_write.elapsed() >= METRICS_HEARTBEAT;
        if self.last_metrics.as_deref() == Some(content.as_str()) && !heartbeat {
            return;
        }
        let metrics = ExtMetrics {
            version: METRICS_VERSION,
            updated_ms: unix_ms(),
            extensions,
        };
        match metrics.save(&state::paths().metrics()) {
            Ok(()) => {
                self.last_metrics = Some(content);
                self.last_metrics_write = Instant::now();
                self.metrics_failed = false;
            }
            Err(error) => {
                if !self.metrics_failed {
                    eprintln!("扩展指标写入失败: {error}");
                    self.metrics_failed = true;
                }
            }
        }
    }
}

/// 同意记录的轻量指纹:文件出现或重写就重建运行期,不用重启进程。
fn consent_fingerprint() -> Option<u64> {
    let bytes = std::fs::read(state::paths().consent()).ok()?;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(hash)
}

/// 清单已知但没进运行期的包:状态可展示,指标都从零开始。
fn base_stat(
    manifest: &lueying_ext::host::Manifest,
    state: ExtState,
    last_error: Option<String>,
) -> ExtStat {
    ExtStat {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        state,
        last_error,
        ..ExtStat::default()
    }
}

/// 清单都读不出来的包只留 id 与错误。
fn error_stat(id: &str, error: String) -> ExtStat {
    ExtStat {
        id: id.to_string(),
        state: ExtState::Error,
        last_error: Some(error),
        ..ExtStat::default()
    }
}

/// Unix 毫秒;扩展的 `lueying.timeout` 以它为宿主时钟。
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
