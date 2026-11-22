//! 模式徽标:Rune overlay。中英/全半切换时在光标旁短暂显示「中」「英」「全」「半」。
//!
//! 与候选窗同一个 App、同一条线程;imp 侧 touch_hud 递增序号并 kick 唤醒,
//! 摆位沿用旧 D2D 徽标的避让规则(候选窗在同侧就翻到另一侧,前台全屏不弹)。
//! 节奏:淡入、保持 600ms、淡出 180ms;系统关掉动画效果时直接显隐。
//! 画面配方与 `lueying_render::badge` 一致,配色与候选窗同源。
//!
//! 注意:回落候选(LUEYING_CANDIDATE_COMPOSITION=1)不走 Rune 线程,该模式下
//! 徽标不显示——回落路径本身就是为了在 Rune 管线不可用时保打字。

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lueying_render::badge;
use lueying_server::theme;
use rune::color::srgb_decode;
use rune::{
    Color, Cx, El, KeyPolicy, OverlayHandle, OverlayInput, OverlayOptions, OverlaySpec, TextStyle,
    Transition, Tx, el, text,
};

use crate::imp::State;
use crate::ui::place;

/// 淡入后的保持时长与淡出时长(与旧 D2D 徽标一致)。
const HUD_SHOW: Duration = Duration::from_millis(600);
const HUD_FADE: Duration = Duration::from_millis(180);
/// 徽标与光标之间的距离(逻辑像素)。
const GAP: f32 = 4.0;
/// 徽标字号(逻辑像素),与 `lueying_render::badge` 的配方一致。
const TEXT_SIZE: f32 = 17.0;
/// 徽标圆角半径(逻辑像素),与 `lueying_render::badge` 的配方一致。
const RADIUS: f32 = 8.0;

static HANDLE: OnceLock<OverlayHandle<BadgeHud>> = OnceLock::new();

/// 徽标 overlay 的状态:共享 State 的只读快照加当前这份视图。
pub(super) struct BadgeHud {
    state: Arc<State>,
    /// 上次看到的切换序号;变了才重读徽标。
    serial: u64,
    current: Option<HudView>,
    /// 外观与主题色下标按切换取一次:系统主题切得很慢,每帧读注册表纯属浪费。
    appearance: theme::Appearance,
    accent: usize,
    shown: bool,
    /// 保持期结束,已排队整窗淡出(或直接隐藏)。
    done: bool,
}

/// 当前这份徽标:显示哪个字、切换时刻。
#[derive(Clone)]
struct HudView {
    label: &'static str,
    /// 切换时的宿主光标矩形(物理像素);没有可用光标时退到主屏。
    caret: Option<(i32, i32, i32, i32)>,
    at: Instant,
}

impl BadgeHud {
    fn new(state: Arc<State>) -> Self {
        Self {
            state,
            serial: 0,
            current: None,
            appearance: theme::Appearance::of_system(),
            accent: 0,
            shown: false,
            done: false,
        }
    }
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
    .release_gpu_when_hidden()
    .poll(badge_poll);
    let handle = app.overlay(overlay, BadgeHud::new(state));
    let _ = HANDLE.set(handle);
}

/// 切换序号变了就重读徽标;保持期结束排整窗淡出,总时长到期停靠。
fn badge_poll(s: &mut BadgeHud, _now: f64, tx: &mut Tx) -> bool {
    let serial = s.state.hud_serial.load(Ordering::Acquire);
    if serial != s.serial {
        s.serial = serial;
        s.current = hud_view(&s.state);
        s.appearance = theme::Appearance::of_system();
        s.accent = place::ui_options(&s.state).2;
        s.shown = false;
        s.done = false;
    }
    let Some(view) = s.current.clone() else {
        return false;
    };
    let elapsed = Instant::now().saturating_duration_since(view.at);
    if elapsed >= HUD_SHOW + HUD_FADE {
        // 兜底停靠:fade_out 的窗口级淡出走完会自动隐藏,这里只清状态。
        s.current = None;
        if let Some(handle) = HANDLE.get() {
            handle.hide();
        }
        return false;
    }
    if s.done {
        // 淡出已排队,等它走完自动隐藏;不占唤醒。
        return false;
    }
    match place(&s.state, &view) {
        Some(rect) => {
            if let Some(handle) = HANDLE.get() {
                handle.set_placement(Some(super::rune::placement_rect(
                    rect,
                    place::scale_at(rect.0, rect.1),
                )));
                if !s.shown {
                    handle.show();
                    s.shown = true;
                }
                if elapsed >= HUD_SHOW {
                    s.done = true;
                    if reduce_motion() {
                        handle.hide();
                    } else {
                        handle.fade_out(HUD_FADE);
                        // 淡出走完自动隐藏;排一次唤醒做停靠兜底。
                        tx.wake_after(HUD_FADE + Duration::from_millis(40));
                    }
                }
            }
        }
        None => {
            // 前台全屏:不弹徽标。
            s.current = None;
            if let Some(handle) = HANDLE.get() {
                handle.hide();
            }
            return false;
        }
    }
    if elapsed < HUD_SHOW {
        tx.wake_after(HUD_SHOW - elapsed);
    }
    false
}

/// 画一枚徽标:圆角卡片 + 描边 + 居中的字,配方与 `lueying_render::badge` 一致。
fn badge_view(s: &BadgeHud, cx: &mut Cx<BadgeHud>) -> El {
    // 系统关掉"动画效果"时,入场淡入降级为直接出现。
    cx.reduced_motion(!lueying_server::theme::client_area_animation());
    let Some(view) = s.current.as_ref() else {
        return el();
    };
    let palette = theme::palette(theme::colors_with(s.appearance, s.accent));
    let high_contrast = matches!(s.appearance, theme::Appearance::HighContrast);
    let mut card = el()
        .size(badge::WIDTH, badge::HEIGHT)
        .radius(RADIUS)
        .fill(rgb(palette.bg))
        .border(1.0, rgb(palette.border))
        .center()
        .child(
            text(view.label)
                .font(TextStyle::new(TEXT_SIZE, 400.0))
                .color(rgb(palette.text)),
        );
    if !high_contrast {
        // 轻投影把徽标从宿主窗口上托起来。
        card = card.shadow(0.0, 6.0, rgba(0x000000, 28.0 / 255.0));
    }
    // 每份徽标一个 key:重挂载重播入场淡入。
    card.key(format!("hud-{}", s.serial))
        .transition(Transition::opacity())
}

/// 读当前这份徽标;没有就清空。
fn hud_view(state: &State) -> Option<HudView> {
    let source = state.mode_hud.lock().unwrap_or_else(|p| p.into_inner());
    source.as_ref().map(|hud| HudView {
        label: hud.label,
        caret: hud.caret,
        at: hud.at,
    })
}

/// 徽标摆位:平时贴在光标下方;候选窗在同一侧时翻到另一侧,两者不互相遮挡。
/// 返回 (x, y, 宽, 高)(物理像素);前台全屏时不弹。
fn place(state: &State, view: &HudView) -> Option<(i32, i32, i32, i32)> {
    let caret = view
        .caret
        .map(|(l, t, r, b)| (l.min(r), t.min(b), l.max(r), b.max(t)));
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

/// 减少动效的系统开关(与候选窗、设置窗共用一份)。
fn reduce_motion() -> bool {
    !lueying_server::theme::client_area_animation()
}

/// 0xRRGGBB → Rune 颜色(线性 sRGB)。
fn rgb(v: u32) -> Color {
    let channel = |shift: u32| srgb_decode(((v >> shift) & 0xff) as f32 / 255.0);
    Color {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: 1.0,
    }
}

/// 0xRRGGBB + 线性透明度。
fn rgba(v: u32, a: f32) -> Color {
    let mut color = rgb(v);
    color.a = a;
    color
}
