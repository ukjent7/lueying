//! 模式徽标:Rune overlay。中英/全半切换时在光标旁短暂显示当前模式。
//!
//! 与候选窗同一个 App、同一条线程;imp 侧 touch_hud 递增序号并 kick 唤醒。
//! 入场/退场交给框架:`animate_reveal` 让 show 播放插入过渡,保持期结束撤下
//! 内容时移除过渡接管淡出,播完再隐藏窗口。摆位沿用避让规则(候选窗在同侧
//! 就翻到另一侧,前台全屏不弹)。画面用 kit 的 Badge 组件,配色从主题三色
//! 种子派生,高对比度由组件自己处理。
//!
//! 注意:回落候选(LUEYING_CANDIDATE_COMPOSITION=1)不走 Rune 线程,该模式下
//! 徽标不显示——回落路径本身就是为了在 Rune 管线不可用时保打字。

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lueying_render::badge;
use lueying_server::theme;
use rune::{
    Color, Cx, El, KeyPolicy, OverlayHandle, OverlayInput, OverlayOptions, OverlaySpec, Transition,
    Tx, el,
};
use rune_kit::{Badge, BadgeVariant, Kit, PaletteSeed};

use crate::imp::State;
use crate::ui::place;

/// 保持时长与退场淡出的等待余量。
const HOLD: Duration = Duration::from_millis(600);
const EXIT: Duration = Duration::from_millis(300);
/// 徽标与光标之间的距离(逻辑像素)。
const GAP: f32 = 4.0;

static HANDLE: OnceLock<OverlayHandle<BadgeHud>> = OnceLock::new();

/// 徽标 overlay 的状态:共享 State 的只读快照加当前这份视图。
pub(super) struct BadgeHud {
    state: Arc<State>,
    /// 上次看到的切换序号;变了才重读徽标。
    serial: u64,
    label: Option<&'static str>,
    /// 切换时的宿主光标矩形(物理像素);没有可用光标时退到主屏。
    caret: Option<(i32, i32, i32, i32)>,
    /// 本次切换的帧钟起点(秒);保持期与退场淡出都按它算。
    at: f64,
    /// 外观与主题色下标按切换取一次:系统主题切得很慢,每帧读注册表纯属浪费。
    appearance: theme::Appearance,
    accent: usize,
    kit: Kit,
    shown: bool,
    /// 保持期结束,内容已撤下,退场淡出在播。
    exit: bool,
}

impl BadgeHud {
    fn new(state: Arc<State>) -> Self {
        let appearance = theme::Appearance::of_system();
        Self {
            state,
            serial: 0,
            label: None,
            caret: None,
            at: 0.0,
            appearance,
            accent: 0,
            kit: kit(appearance, 0),
            shown: false,
            exit: false,
        }
    }
}

/// 主题三色种子派生 kit 令牌,徽标与设置窗同一套配色来源。
fn kit(appearance: theme::Appearance, accent: usize) -> Kit {
    let colors = theme::colors_with(appearance, accent);
    Kit::from_palette(PaletteSeed {
        bg: rgb(colors.bg),
        fg: rgb(colors.text),
        accent: rgb(colors.accent),
    })
}

/// 徽标摆位:平时贴在光标下方;候选窗在同一侧时翻到另一侧,两者不互相遮挡。
/// 返回 (x, y, 宽, 高)(物理像素);前台全屏时不弹。
fn place(state: &State, caret: Option<(i32, i32, i32, i32)>) -> Option<(i32, i32, i32, i32)> {
    let caret = caret.map(|(l, t, r, b)| (l.min(r), t.min(b), l.max(r), b.max(t)));
    let host_pid = (state.active.load(Ordering::Relaxed) >> 32) as u32;
    let (anchor_x, anchor_y, work) = match caret {
        Some((l, _, r, b)) => (l + (r - l) / 2, b, place::work_area_at(l, b)),
        None => {
            let work = place::primary_work_area();
            (
                work.left + (work.right - work.left) / 2,
                work.top + (work.bottom - work.top) * 2 / 3,
                work,
            )
        }
    };
    if place::foreground_fullscreen_at(anchor_x, anchor_y, host_pid) {
        return None;
    }
    let scale = place::scale_at(anchor_x, anchor_y);
    let w = (badge::WIDTH * scale) as i32;
    let h = (badge::HEIGHT * scale) as i32;
    let gap = (GAP * scale) as i32;
    // 候选窗在哪一侧,徽标就去另一侧。
    let composing = place::in_composition(state);
    let candidate_above = state.candidate_above.load(Ordering::Relaxed);
    let prefer_below = !composing || !candidate_above;
    let x = (anchor_x - w / 2).clamp(work.left, (work.right - w).max(work.left));
    let y = (match caret {
        Some((_, t, _, b)) => {
            let below = b + gap;
            let above = t - h - gap;
            if prefer_below && below + h <= work.bottom {
                below
            } else if !prefer_below && above >= work.top {
                above
            } else if below + h <= work.bottom {
                below
            } else {
                above
            }
        }
        None => anchor_y - h / 2,
    })
    .clamp(work.top, (work.bottom - h).max(work.top));
    Some((x, y, w, h))
}

/// 把徽标 overlay 注册进候选窗的 App;窗口在首次 show 时才建。
pub(super) fn register<T: 'static>(app: &mut rune::App<T>, state: Arc<State>) {
    let overlay = OverlaySpec::new(
        "lueying-mode-hud",
        (badge::WIDTH, badge::HEIGHT),
        badge_view,
    )
    .options(OverlayOptions {
        // 徽标是纯指示:不吃按键,指针全穿透,隐藏即还 GPU 表面。
        keys: KeyPolicy::None,
        input: OverlayInput::None,
        ..Default::default()
    })
    // show 播放入场过渡,而不是直接落在最终帧。
    .animate_reveal()
    .release_gpu_when_hidden()
    .poll(badge_poll);
    let handle = app.overlay(overlay, BadgeHud::new(state));
    let _ = HANDLE.set(handle);
}

/// 管道线程叫醒徽标:切换序号变了,徽标自己的 poll 会读到并重绘。
/// 徽标是候选窗 App 里的独立 overlay,候选窗的 kick 到不了它。
pub(super) fn kick() {
    if let Some(handle) = HANDLE.get() {
        handle.update(|_: &mut BadgeHud, _| {});
    }
}

/// 切换序号变了就重读徽标;保持期结束撤下内容,退场淡出播完再停靠。
/// poll 返回 true 才会让 view 重跑(上游契约:false 即不重跑、不重绘),
/// 所以序号变化与撤下内容都必须返回 true。
fn badge_poll(s: &mut BadgeHud, now: f64, tx: &mut Tx) -> bool {
    let serial = s.state.hud_serial.load(Ordering::Acquire);
    let mut changed = false;
    if serial != s.serial {
        s.serial = serial;
        let source = s.state.mode_hud.lock().unwrap_or_else(|p| p.into_inner());
        s.label = source.as_ref().map(|hud| hud.label);
        s.caret = source.as_ref().and_then(|hud| hud.caret);
        s.at = now;
        s.appearance = theme::Appearance::of_system();
        s.accent = place::ui_options(&s.state).2;
        s.kit = kit(s.appearance, s.accent);
        s.shown = false;
        s.exit = false;
        changed = true;
    }
    let elapsed = (now - s.at).max(0.0);
    if elapsed >= (HOLD + EXIT).as_secs_f64() {
        // 退场淡出播完(窗口级 hide 只是兜底收尾):停靠。
        s.label = None;
        s.caret = None;
        s.shown = false;
        if let Some(handle) = HANDLE.get() {
            handle.hide();
        }
        return changed;
    }
    if !s.shown {
        let Some(rect) = place(&s.state, s.caret) else {
            // 前台全屏:这份不弹,等下一份。
            s.label = None;
            s.caret = None;
            return changed;
        };
        if let Some(handle) = HANDLE.get() {
            handle.set_placement(Some(super::rune::placement_rect(
                rect,
                place::scale_at(rect.0, rect.1),
            )));
            handle.show();
        }
        s.shown = true;
        changed = true;
    }
    if elapsed < HOLD.as_secs_f64() {
        tx.wake_at(s.at + HOLD.as_secs_f64());
        return changed;
    }
    if !s.exit {
        // 保持期结束:撤下内容。返回 true 让 view 重跑,徽标元素卸载,
        // 移除过渡接管退场淡出,播完等 EXIT 到期由上面的分支停靠。
        s.label = None;
        s.caret = None;
        s.exit = true;
        tx.wake_after(EXIT);
        return true;
    }
    changed
}

/// 一枚徽标:kit 的 Badge 组件居中在透明 overlay 里;每份一个 key,重挂载重播入场。
fn badge_view(s: &BadgeHud, cx: &mut Cx<BadgeHud>) -> El {
    // 系统关掉"动画效果"时,过渡降级为框架内置的快速淡出;上游宿主
    // 喂的就是同一个 SPI,不重复读注册表。
    cx.reduced_motion(cx.system_reduced_motion());
    if s.exit || s.label.is_none() {
        return el();
    }
    let Some(label) = s.label else { return el() };
    let high_contrast = matches!(s.appearance, theme::Appearance::HighContrast);
    el().size(badge::WIDTH, badge::HEIGHT)
        .center()
        .child({
            let mut pill = Badge::new(label).variant(BadgeVariant::Accent).standard();
            if high_contrast {
                pill = pill.high_contrast();
            }
            pill.build(&s.kit)
        })
        .key(format!("hud-{}", s.serial))
        .transition(Transition::opacity())
}

/// 0xRRGGBB → Rune 颜色(线性 sRGB);转换共享一份,见 theme::srgb。
fn rgb(v: u32) -> Color {
    theme::srgb(v)
}
