//! 运行期脚本扩展:后台线程跑 QuickJS,窗口线程只收主题色与 SkSL 源码。
//!
//! 派发在独立线程里,窗口线程只做 `submit` 与 `take`:脚本再慢也压不住
//! 打字反馈。补丁只在内容变化时重绘一帧;坏源码只废掉自己的槽位,
//! 高对比度下效果整个关掉,密码/隐私字段不派发。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lueying_extjs::{EffectSlot, EffectsPatch, Pack, Poll, SceneIn, ScenePatch};
use lueying_protocol::SessionView;
use lueying_render::effect::{EffectSet, Slot, Uniforms};
use lueying_server::config;
use lueying_server::theme;
use skia_safe::Shader;

use crate::imp::State;
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

/// 脚本线程:跟配置换包、跟目录热重载、按最新场景派发。
fn worker(
    state: Arc<State>,
    latest: Arc<Mutex<Option<(u64, SceneIn)>>>,
    wake: Receiver<()>,
    patches: Sender<(u64, Option<ScenePatch>)>,
) {
    let mut id = String::new();
    let mut pack: Option<Pack> = None;
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
            if want != id {
                id = want;
                pack = load(&id);
                reported = None;
                // 换包或关包先摘掉旧效果,等新场景的补丁。
                let _ = patches.send((last_seq, None));
            }
        }
        if now >= next_poll {
            next_poll = now + POLL;
            let mut unloaded = false;
            if let Some(current) = pack.as_mut() {
                match current.poll() {
                    Poll::Unchanged => {}
                    Poll::Reloaded => {
                        println!("扩展 {} 已重载", current.manifest().id);
                        reported = None;
                    }
                    Poll::Failed(e) => {
                        println!("扩展 {} 重载失败,继续用上一版: {e}", current.manifest().id)
                    }
                    Poll::Removed => {
                        println!("扩展 {} 已卸载(清单被删)", current.manifest().id);
                        unloaded = true;
                    }
                }
            }
            if unloaded {
                pack = None;
                let _ = patches.send((last_seq, None));
            }
        }
        let Some((seq, scene)) = latest.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            continue;
        };
        last_seq = seq;
        let Some(current) = pack.as_mut() else {
            // 没启用扩展也要回一条清空,别让上一个包的装饰留在窗口上。
            let _ = patches.send((seq, None));
            continue;
        };
        let patch = current.dispatch(&scene);
        match current.last_error() {
            Some(error) if reported.as_deref() != Some(error) => {
                println!("扩展 {} 运行失败: {error}", current.manifest().id);
                reported = Some(error.to_string());
            }
            None => reported = None,
            Some(_) => {}
        }
        let _ = patches.send((seq, patch));
    }
}

/// 配置里生效的扩展 id;空表示不启用。
fn configured(state: &State) -> String {
    state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .script
        .clone()
}

/// 按 id 加载扩展包;目录名必须等于清单里的 id,由 `Pack` 检查。
fn load(id: &str) -> Option<Pack> {
    if id.is_empty() {
        return None;
    }
    let Some(root) = config::extensions_dir() else {
        println!("扩展 {id} 不可用:APPDATA 缺失");
        return None;
    };
    match Pack::load(&root.join(id)) {
        Ok(pack) => {
            let manifest = pack.manifest();
            println!(
                "扩展 {} 已加载({} {})",
                manifest.name, manifest.id, manifest.version
            );
            Some(pack)
        }
        Err(e) => {
            println!("扩展 {id} 加载失败: {e}");
            None
        }
    }
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
