//! 运行期脚本扩展:后台线程跑 QuickJS,窗口线程只收主题色与 SkSL 源码。
//!
//! 派发在独立线程里,窗口线程只做 `submit` 与 `take`:脚本再慢也压不住
//! 打字反馈。补丁只在内容变化时重绘一帧;坏源码只废掉自己的槽位,
//! 高对比度下效果整个关掉,密码/隐私字段不派发。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lueying_extjs::bindings::BindingTable;
use lueying_extjs::{
    EffectSlot, EffectsPatch, Invoke, Limits, Registry, Runtime, SceneIn, ScenePatch, Service,
};
use lueying_protocol::SessionView;
use lueying_render::effect::{EffectSet, Slot, Uniforms};
use lueying_server::config;
use lueying_server::services;
use lueying_server::theme;
use skia_safe::Shader;

use crate::imp::{ExtHub, State};
use crate::ui_place;

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
    // 动作候选:请求线程按完整命令 id 投递,这里执行;
    // 标题表同时供请求线程判断命令是否存在。
    let (action_tx, action_rx) = mpsc::channel::<String>();
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
            let order_changed = want != order;
            if order_changed {
                order = want;
                // 旧扩展的注册物随 Scope 消失;热键的 OS 注册在这里一并清掉。
                hotkey.reset();
                runtime = load(&order, &hotkey);
                reported = None;
                // 换扩展或关扩展先摘掉旧效果,等新场景的补丁。
                let _ = patches.send((last_seq, None));
            }
            if order_changed || want_binds != bound {
                bound = want_binds;
                bindings = bind_rows(&state, runtime.as_ref(), &hotkey);
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
            // 动作候选选中的命令:与热键走同一条执行路径,失败只写日志。
            while let Ok(command) = action_rx.try_recv() {
                run_command(current, &command, "动作候选");
            }
            // 热键触发与异步命令推进;没有热键和挂起任务时都是微秒级空检查。
            dispatch_hotkey(&state, current, &hotkey, &bindings);
            let _ = current.pump(unix_ms());
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
        if let Err(error) = table.insert(&combo, command, info.when) {
            println!("绑定 {combo} 跳过:{error}");
            continue;
        }
        let row = table.rows().len() - 1;
        if let Err(error) = hotkey.bind(row, &combo) {
            println!("绑定 {combo} 注册失败:{error}");
        }
    }
    table
}

/// 收热键触发:扩展 press 已在 pump 里就地兑现;用户绑定行按 when 过滤后
/// 执行,结果与错误都写日志。破坏性确认与命令面板入口随 T4 后续任务接上。
fn dispatch_hotkey(
    state: &State,
    runtime: &mut Runtime,
    hotkey: &services::HotkeyProvider,
    bindings: &BindingTable,
) {
    let fired = hotkey.pump();
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
        );
    }
}

/// 执行一条扩展命令并泵几轮:快命令当次结算,等服务与长定时器的由循环节拍收尾。
fn run_command(runtime: &mut Runtime, command: &str, source: &str) {
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

/// Unix 毫秒;扩展的 lueying.timeout 以它为宿主时钟。
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

/// 按配置顺序打开扩展;坏包只记日志,不影响其他扩展。
/// 服务缝装上 Windows 提供者;output/panel 是 T6/T4b 的占位,回明确错误。
/// 热键提供者由 worker 持有,换扩展时其注册整表重建,不随运行期重建。
fn load(order: &[String], hotkey: &Arc<services::HotkeyProvider>) -> Option<Runtime> {
    if order.is_empty() {
        return None;
    }
    let Some(root) = config::extensions_dir() else {
        println!("扩展不可用:APPDATA 缺失");
        return None;
    };
    let registry = Arc::new(Registry::new());
    registry.set(Service::Capture, Arc::new(services::CaptureProvider));
    registry.set(Service::Ocr, Arc::new(services::OcrProvider));
    registry.set(Service::Output, Arc::new(services::OutputProvider));
    registry.set(Service::Panel, Arc::new(services::PanelProvider));
    // 热键提供者由 worker 持有共享状态;`set` 收 trait 对象,先落到具体类型再强转。
    let hotkey: Arc<services::HotkeyProvider> = Arc::clone(hotkey);
    registry.set(Service::Hotkey, hotkey);
    registry.set(Service::Notify, Arc::new(services::NotifyProvider));
    let (runtime, errors) = Runtime::open_with(&root, order, Limits::default(), registry);
    for error in errors {
        println!("{error}");
    }
    println!("运行期扩展:{} 个槽位", order.len());
    Some(runtime)
}

/// 面板状态转场景;只含几何与主题,没有候选文字与宿主名。
pub(super) fn scene(
    state: &State,
    view: &SessionView,
    private: bool,
    reduce_motion: bool,
    time: f32,
    anim: f32,
) -> SceneIn {
    let (_, _, accent) = ui_place::ui_options(state);
    let appearance = theme::Appearance::of_system();
    let colors = theme::colors_with(appearance, accent);
    let page_size = view.page_size.max(1);
    let start = view.page * page_size;
    let candidates = view.candidates.len().saturating_sub(start).min(page_size);
    let selected = if candidates == 0 {
        0
    } else {
        view.highlight.saturating_sub(start).min(candidates - 1)
    };
    SceneIn {
        composing: true,
        page: (view.page + 1) as u32,
        pages: view.candidates.len().div_ceil(page_size).max(1) as u32,
        candidates: candidates as u32,
        selected: selected as u32,
        accent: colors.accent,
        dark: matches!(appearance, theme::Appearance::Dark),
        high_contrast: matches!(appearance, theme::Appearance::HighContrast),
        reduce_motion,
        time,
        anim,
        private,
        ..SceneIn::default()
    }
}

/// 已应用的脚本补丁:主题色进调色板,效果编译成 Skia 对象。
pub(super) struct ScriptFx {
    patch: ScenePatch,
    effects: Option<EffectSet>,
}

impl ScriptFx {
    /// 编译一份新补丁;效果源码没变就复用上一份已编译的对象。
    pub(super) fn update(previous: Option<Self>, patch: ScenePatch) -> Self {
        let effects = match previous {
            Some(mut old) if old.patch.effects == patch.effects => old.effects.take(),
            _ => patch.effects.as_ref().map(compile),
        };
        Self { patch, effects }
    }

    /// 补丁里的主题色;没给就沿用设置。
    pub(super) fn accent(&self) -> Option<u32> {
        self.patch.accent
    }

    /// 生成脚本效果的 shader;没给或编译失败都返回 `None`。
    pub(super) fn shader(
        &self,
        slot: Slot,
        uniforms: &Uniforms,
        origin: (f32, f32),
        unit: f32,
    ) -> Option<Shader> {
        self.effects.as_ref()?.shader(slot, uniforms, origin, unit)
    }

    /// 与另一份补丁是否完全相同;相同就不重绘。
    pub(super) fn same(&self, other: &ScenePatch) -> bool {
        &self.patch == other
    }
}

/// 编译补丁里的效果源码;坏源码只废掉自己的槽位,错误写日志。
fn compile(sources: &EffectsPatch) -> EffectSet {
    let set = EffectSet::from_sources(sources.iter().map(|(slot, source)| (slot_of(slot), source)));
    for error in set.errors() {
        println!("扩展效果:{error}");
    }
    set
}

/// 扩展槽位转渲染槽位。
fn slot_of(slot: EffectSlot) -> Slot {
    match slot {
        EffectSlot::Background => Slot::Background,
        EffectSlot::Highlight => Slot::Highlight,
        EffectSlot::Menu => Slot::Menu,
    }
}
