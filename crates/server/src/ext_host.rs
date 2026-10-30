//! 运行期脚本扩展宿主:独立线程跑 QuickJS、命令、热键与服务缝。
//!
//! 与候选窗解耦:补丁经通道交给窗口线程,窗口线程只做 `submit` 与 `take`;
//! 没有 UI 时(Windows E2E、无桌面会话)宿主照常跑,补丁直接丢弃。
//! 扩展目录与生效列表按内容/配置轮询,换扩展或热重载都不重启进程。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lueying_extjs::bindings::BindingTable;
use lueying_extjs::{
    ConsentBook, ConsentGate, Invoke, Limits, PrivacyCheck, Registry, Runtime, SceneIn, ScenePatch,
    Service,
};
use lueying_server::command_panel::CommandItem;
use lueying_server::config;
use lueying_server::services;

use crate::ext_audit::FileAudit;
use crate::imp::{ExtHub, ExtInvoke, State};

/// 扩展目录的轮询间隔;改完脚本一秒内生效。
const POLL: Duration = Duration::from_millis(1000);
/// 生效扩展 id 的检查间隔;设置窗保存后不用重启。
const CONFIG: Duration = Duration::from_millis(500);

/// 窗口线程持有的脚本通道。
pub(super) struct Script {
    /// 最新场景与序号;脚本忙时新场景覆盖旧场景,只补最后一帧。
    latest: Arc<Mutex<Option<(u64, SceneIn)>>>,
    /// 提交序号;晚到的旧补丁按它丢弃,不会盖到新一帧或隐私字段上。
    seq: AtomicU64,
    /// 唤醒脚本线程;容量 1,已有唤醒就跳过。
    wake: SyncSender<()>,
    /// 脚本回的补丁;窗口线程每轮取最后一条。
    patches: Receiver<(u64, Option<ScenePatch>)>,
}

impl Script {
    /// 起脚本线程;线程起不来时脚本层静默失效,输入法照常。
    pub(super) fn spawn(state: Arc<State>) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let (wake, wake_rx) = mpsc::sync_channel(1);
        let (patches_tx, patches) = mpsc::channel();
        let worker_latest = Arc::clone(&latest);
        let spawned = std::thread::Builder::new()
            .name("ext-script".into())
            .spawn(move || worker(state, worker_latest, wake_rx, patches_tx));
        if let Err(e) = spawned {
            println!("扩展线程启动失败: {e}");
        }
        Self {
            latest,
            seq: AtomicU64::new(0),
            wake,
            patches,
        }
    }

    /// 提交最新场景;脚本线程忙时只保留最后一帧。
    pub(super) fn submit(&self, scene: SceneIn) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        *self.latest.lock().unwrap_or_else(|p| p.into_inner()) = Some((seq, scene));
        let _ = self.wake.try_send(());
    }

    /// 已提交的最新场景序号;比它旧的补丁直接丢弃。
    pub(super) fn seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// 取走最新补丁;外层 `None` 表示没有新补丁,内层 `None` 表示清空效果。
    pub(super) fn take(&self) -> Option<(u64, Option<ScenePatch>)> {
        let mut latest = None;
        while let Ok(patch) = self.patches.try_recv() {
            latest = Some(patch);
        }
        latest
    }
}

/// 脚本线程:跟配置换扩展、跟目录热重载、按最新场景派发。
fn worker(
    state: Arc<State>,
    latest: Arc<Mutex<Option<(u64, SceneIn)>>>,
    wake: Receiver<()>,
    patches: Sender<(u64, Option<ScenePatch>)>,
) {
    let mut order: Vec<String> = Vec::new();
    let mut runtime: Option<Runtime> = None;
    let hotkey = Arc::new(services::HotkeyProvider::default());
    let mut bindings = BindingTable::default();
    let mut bound: Vec<(String, String)> = Vec::new();
    // 命令面板总热键的当前组合;空串表示关闭。
    let mut panel_bound = String::new();
    // 当前活动会话是否在密码/隐私字段里;服务缝与热键派发都按它停用。
    let privacy: PrivacyCheck = {
        let state = Arc::clone(&state);
        Arc::new(move || {
            let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions
                .get(&state.active.load(Ordering::Relaxed))
                .is_some_and(|session| session.private)
        })
    };
    // 同意记录的最后指纹;用户同意后不重启进程,整表重建运行期。
    let mut consent_stamp: Option<u64> = None;
    // 动作候选:请求线程按完整命令 id 投递,这里执行;
    // 标题表同时供请求线程判断命令是否存在。
    let (action_tx, action_rx) = mpsc::channel::<ExtInvoke>();
    let titles: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    *state.ext_hub.lock().unwrap_or_else(|p| p.into_inner()) = Some(ExtHub {
        invoke: action_tx,
        titles: Arc::clone(&titles),
    });
    let mut reported: Option<String> = None;
    let mut last_seq = 0;
    let mut next_config = Instant::now();
    let mut next_poll = Instant::now();
    loop {
        let _ = wake.recv_timeout(Duration::from_millis(100));
        let now = Instant::now();
        if now >= next_config {
            next_config = now + CONFIG;
            let want = configured(&state);
            let want_binds = configured_bindings(&state);
            let stamp = consent_fingerprint();
            let consent_changed = stamp != consent_stamp;
            consent_stamp = stamp;
            let order_changed = want != order || consent_changed;
            if order_changed {
                order = want;
                // 旧扩展的注册物随 Scope 消失;热键的 OS 注册在这里一并清掉。
                hotkey.reset();
                panel_bound.clear();
                runtime = load(&order, &hotkey, &privacy);
                reported = None;
                // 换扩展或关扩展先摘掉旧效果,等新场景的补丁。
                let _ = patches.send((last_seq, None));
            }
            if order_changed || want_binds != bound {
                bound = want_binds;
                bindings = bind_rows(&state, runtime.as_ref(), &hotkey);
            }
            // 命令面板总热键独立于扩展命令:没有命令时也能唤出空面板。
            let want_panel = configured_panel_hotkey(&state);
            if want_panel != panel_bound {
                hotkey.unbind_panel();
                if !want_panel.is_empty() {
                    match hotkey.bind_panel(&want_panel) {
                        Ok(()) => println!("命令面板热键 {want_panel}"),
                        Err(error) => println!("命令面板热键 {want_panel} 注册失败:{error}"),
                    }
                }
                panel_bound = want_panel;
            }
            // 扩展重载会换掉命令集;标题表每轮刷新,触发词不会指向已卸载的命令。
            refresh_titles(&titles, runtime.as_ref());
        }
        if now >= next_poll {
            next_poll = now + POLL;
            if let Some(current) = runtime.as_mut() {
                let removed = current.poll();
                current.idle_unload();
                if removed > 0 {
                    let _ = patches.send((last_seq, None));
                }
                for id in &order {
                    let Some(error) = current.error(id) else {
                        continue;
                    };
                    let message = format!("{id}: {error}");
                    if reported.as_deref() != Some(&message) {
                        println!("扩展 {id} 运行失败: {error}");
                        reported = Some(message);
                    }
                }
            }
        }
        if let Some(current) = runtime.as_mut() {
            // 动作候选与命令面板选中的命令:与热键走同一条执行路径,失败只写日志。
            while let Ok(invoke) = action_rx.try_recv() {
                run_command(current, &invoke.command, invoke.source, &privacy);
            }
            // 热键触发与异步命令推进;没有热键和挂起任务时都是微秒级空检查。
            dispatch_hotkey(&state, current, &hotkey, &bindings, &privacy);
            let _ = current.pump(unix_ms());
        } else {
            // 没有运行期扩展时也要收总热键;用户绑定不存在,只排空消息队列。
            let _ = hotkey.pump();
            // 面板在扩展卸载后仍可能投回命令:明确记一笔,不静默丢弃。
            while let Ok(invoke) = action_rx.try_recv() {
                println!("{}跳过:扩展未启用({})", invoke.source, invoke.command);
            }
        }
        if hotkey.take_panel() {
            // 扩展未启用时面板仍可打开,只是没有命令可执行。
            show_panel(&state, runtime.as_ref(), &privacy);
        }
        let Some((seq, scene)) = latest.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            continue;
        };
        last_seq = seq;
        let patch = runtime
            .as_mut()
            .and_then(|runtime| runtime.dispatch(&scene));
        let _ = patches.send((seq, patch));
    }
}

/// 刷新命令标题表:请求线程据此判断触发词对应的命令是否可执行。
fn refresh_titles(titles: &Mutex<HashMap<String, String>>, runtime: Option<&Runtime>) {
    let mut map = titles.lock().unwrap_or_else(|p| p.into_inner());
    map.clear();
    if let Some(runtime) = runtime {
        for info in runtime.commands() {
            map.insert(info.id, info.title);
        }
    }
}

/// 配置里的绑定行;配置变化检查用。
fn configured_bindings(state: &State) -> Vec<(String, String)> {
    state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .bindings
        .clone()
}

/// 配置绑定行 → 建绑定表并注册热键;命令不存在或组合不合法的行写日志跳过。
/// 行号即 [`services::HotkeyProvider::bind`] 的注册序号,触发事件按它回带。
fn bind_rows(
    state: &State,
    runtime: Option<&Runtime>,
    hotkey: &services::HotkeyProvider,
) -> BindingTable {
    let rows = configured_bindings(state);
    let mut table = BindingTable::default();
    // 清空的绑定也要摘掉旧注册,否则组合键仍被输入法吃掉。
    hotkey.unbind_users();
    if rows.is_empty() {
        return table;
    }
    let Some(runtime) = runtime else {
        println!("扩展未启用,{} 条绑定忽略", rows.len());
        return table;
    };
    for (combo, command) in rows {
        let Some(info) = runtime.command(&command) else {
            println!("绑定 {combo} 跳过:没有命令 {command}");
            continue;
        };
        if let Err(error) = table.insert(&combo, command.clone(), info.when) {
            println!("绑定 {combo} 跳过:{error}");
            continue;
        }
        let row = table.rows().len() - 1;
        match hotkey.bind(row, &combo) {
            Ok(()) => println!("绑定 {combo} → {command}"),
            Err(error) => println!("绑定 {combo} 注册失败:{error}"),
        }
    }
    table
}

/// 配置里的命令面板总热键;空串表示关闭。
fn configured_panel_hotkey(state: &State) -> String {
    state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .panel_hotkey
        .trim()
        .to_string()
}

/// 唤出命令面板:命令表在这里取一次,面板开着时再按热键直接丢弃。
fn show_panel(state: &State, runtime: Option<&Runtime>, privacy: &PrivacyCheck) {
    if state.panel_open.load(Ordering::Relaxed) {
        return;
    }
    if privacy() {
        println!("命令面板跳过:当前是密码/隐私字段");
        return;
    }
    let items = runtime
        .map(|runtime| {
            runtime
                .commands()
                .into_iter()
                .map(|info| CommandItem {
                    id: info.id,
                    title: info.title,
                    extension: info.extension,
                })
                .collect()
        })
        .unwrap_or_default();
    let panel = state.panel.lock().unwrap_or_else(|p| p.into_inner());
    match panel.as_ref() {
        Some(sender) => {
            let _ = sender.try_send(items);
        }
        None => println!("命令面板不可用:UI 未启动"),
    }
}

/// 收热键触发:扩展 press 已在 pump 里就地兑现;用户绑定行按 when 过滤后
/// 执行,结果与错误都写日志。隐私字段下热键直接丢弃,挂起的 press 收到拒绝。
fn dispatch_hotkey(
    state: &State,
    runtime: &mut Runtime,
    hotkey: &services::HotkeyProvider,
    bindings: &BindingTable,
    privacy: &PrivacyCheck,
) {
    let private = privacy();
    if private {
        hotkey.cancel_presses("隐私字段下已停用扩展热键");
    }
    let fired = hotkey.pump();
    if private {
        if !fired.is_empty() {
            println!("热键跳过:当前是密码/隐私字段");
        }
        return;
    }
    if fired.is_empty() {
        return;
    }
    let input = session_input(state);
    for row in fired {
        let Some(binding) = bindings.rows().get(row) else {
            continue;
        };
        if !input.allows(binding.when) {
            println!("热键 {} 跳过:当前输入状态不满足 when", binding.hotkey);
            continue;
        }
        run_command(
            runtime,
            &binding.command,
            &format!("热键 {}", binding.hotkey),
            privacy,
        );
    }
}

/// 执行一条扩展命令并泵几轮:快命令当次结算,等服务与长定时器的由循环节拍收尾。
fn run_command(runtime: &mut Runtime, command: &str, source: &str, privacy: &PrivacyCheck) {
    if privacy() {
        println!("{source} 跳过 {command}:当前是密码/隐私字段");
        return;
    }
    match runtime.invoke(command, "{}") {
        Ok(Invoke::Done(json)) => println!("{source} 执行 {command} → {json}"),
        Ok(Invoke::Pending(_)) => println!("{source} 触发 {command}(异步执行)"),
        Err(error) => println!("{source} 执行失败:{error}"),
    }
    let now_ms = unix_ms();
    for _ in 0..32 {
        if runtime.pending() == 0 {
            break;
        }
        let _ = runtime.pump(now_ms);
    }
}

/// 触发时刻的输入状态:活跃会话在即可编辑,其组合在即组句。
fn session_input(state: &State) -> lueying_extjs::bindings::Input {
    let sessions = state.sessions.lock().unwrap_or_else(|p| p.into_inner());
    let active = sessions.get(&state.active.load(Ordering::Relaxed));
    lueying_extjs::bindings::Input {
        editable: active.is_some(),
        composing: active
            .and_then(|session| session.engine.composition())
            .is_some(),
    }
}

/// Unix 毫秒;扩展的 `lueying.timeout` 以它为宿主时钟。
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 配置里生效的扩展 id,按组合顺序;空表示不启用。
fn configured(state: &State) -> Vec<String> {
    state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .extensions
        .clone()
}

/// 同意记录的轻量指纹:文件出现或重写就重建运行期,不用重启进程。
fn consent_fingerprint() -> Option<u64> {
    let path = config::extensions_consent_path()?;
    let bytes = std::fs::read(path).ok()?;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(hash)
}

/// 按配置顺序打开扩展;坏包只记日志,不影响其他扩展。
/// 只加载已同意的包,热重载前再由 [`ConsentGate`] 复核一次。
/// 服务缝装上 Windows 提供者;热键提供者由 worker 持有,换扩展时其注册整表重建。
fn load(
    order: &[String],
    hotkey: &Arc<services::HotkeyProvider>,
    privacy: &PrivacyCheck,
) -> Option<Runtime> {
    if order.is_empty() {
        return None;
    }
    let Some(root) = config::extensions_dir() else {
        println!("扩展不可用:APPDATA 缺失");
        return None;
    };
    let consent_path = config::extensions_consent_path();
    let book = consent_path
        .as_deref()
        .map(ConsentBook::load)
        .unwrap_or_default();
    let mut approved = Vec::new();
    for id in order {
        let dir = root.join(id);
        let manifest = match lueying_extjs::manifest_at(&dir) {
            Ok(manifest) => manifest,
            Err(error) => {
                println!("扩展 {id} 未加载: {error}");
                continue;
            }
        };
        match book.check(&dir, &manifest) {
            Ok(check) if !check.required || check.approved => approved.push(id.clone()),
            Ok(check) => {
                let sensitive = if check.sensitive { "敏感组合;" } else { "" };
                println!("扩展 {id} 待同意({sensitive}{})", check.items.join("、"));
            }
            Err(error) => println!("扩展 {id} 同意检查失败: {error}"),
        }
    }
    if approved.is_empty() {
        println!("运行期扩展:没有已同意的包");
        return None;
    }
    let registry = Arc::new(Registry::new());
    registry.set_privacy(Arc::clone(privacy));
    if let Some(path) = config::ext_audit_path() {
        let audit = Arc::new(FileAudit::open(path));
        println!("扩展审计 ← {}", audit.path().display());
        registry.set_audit(audit);
    }
    registry.set(Service::Capture, Arc::new(services::CaptureProvider));
    registry.set(Service::Ocr, Arc::new(services::OcrProvider::new()));
    registry.set(Service::Output, Arc::new(services::OutputProvider));
    registry.set(Service::Panel, Arc::new(services::PanelProvider));
    // 热键提供者由 worker 持有共享状态;`set` 收 trait 对象,先落到具体类型再强转。
    let hotkey: Arc<services::HotkeyProvider> = Arc::clone(hotkey);
    registry.set(Service::Hotkey, hotkey);
    registry.set(Service::Notify, Arc::new(services::NotifyProvider));
    let gate: ConsentGate = {
        let path = consent_path;
        Arc::new(move |manifest, dir| {
            let Some(path) = &path else {
                return Ok(());
            };
            let book = ConsentBook::load(path);
            let check = book
                .check(dir, manifest)
                .map_err(|error| format!("同意检查失败: {error}"))?;
            if check.required && !check.approved {
                let sensitive = if check.sensitive { "敏感组合;" } else { "" };
                return Err(format!(
                    "{sensitive}需在设置里同意: {}",
                    check.items.join("、")
                ));
            }
            Ok(())
        })
    };
    let (runtime, errors) =
        Runtime::open_with_gate(&root, &approved, Limits::default(), registry, Some(gate));
    for error in errors {
        println!("{error}");
    }
    println!("运行期扩展:{} 个槽位(已过同意)", approved.len());
    Some(runtime)
}
