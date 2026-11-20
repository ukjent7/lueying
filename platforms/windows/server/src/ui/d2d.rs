//! D2D 候选窗:Win32 无焦点弹出窗 + DirectComposition 合成交换链 + Direct2D。
//!
//! 候选窗:在界面线程的消息循环里渲染,版本号变化与动画帧驱动重绘;摆位、停靠、DPI 与
//! 全屏避让同 `ui::place` 助手共用。出现即落位,不做入场位移动效;交互 parity:
//! 淡出、翻页滑动、点选/滚轮/翻页箭头、右键菜单与悬停,动效可被按键打断,
//! 「减少动态效果」时全部走终态。管线在 `ui::d2d::pipe`:硬件优先,没有适配器退 WARP;设备
//! 被系统摘掉就地整条重建,窗口不换。`LUEYING_D2D_SELFTEST` 供 CI 自检整条
//! 管线。模式徽标走原生分层窗口(`ui::hud`),不在这条管线上。

use std::sync::atomic::Ordering;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use lueying_ext::host::{SceneIn, ScenePatch};
use lueying_ext::proto::candidate::CandidateState;
use lueying_protocol::SessionView;
use lueying_render::panel::{self, Metrics, PANEL_RADIUS, Palette, PanelSpec};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, GWL_EXSTYLE,
    GWLP_USERDATA, GetWindowLongPtrW, GetWindowRect, HCURSOR, HWND_TOPMOST, IDC_ARROW, IDC_HAND,
    LoadCursorW, MSGFLT_ALLOW, RegisterClassW, SPI_GETCLIENTAREAANIMATION, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW,
    WDA_EXCLUDEFROMCAPTURE, WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_SETCURSOR, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{BOOL, w};

mod glass;
mod material;
mod mouse;
mod pipe;
pub(crate) mod selftest;

use crate::imp::State;
use crate::ui::d2d::material::Material;
use crate::ui::d2d::pipe::{Frame, Morph, Pipeline, Target, Text};
use crate::ui::place::{self, CARET_GAP};

/// 淡出/滑动期间的重绘节奏。
const ANIM_TICK: Duration = Duration::from_millis(5);
/// 展开形变按 ~60fps 推进:窗口逐帧改高,60 帧足够顺滑,也少建几次表面。
const EXPAND_TICK: Duration = Duration::from_millis(16);
/// 活背板的重捕节奏(~30fps):背景变动跟随,再高肉眼无感只费电。
const LIVE_TICK: Duration = Duration::from_millis(33);
/// 候选文字最大宽度(逻辑像素):窗口宽度减去页码与翻页箭头的空间。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 组合结束后的淡出时长;期间沿用最后一份内容并让鼠标穿透。
const HIDE_FADE: Duration = Duration::from_millis(90);
/// 翻页时候选内容横向滑入,90ms 内归位。
const PAGE_SLIDE: Duration = Duration::from_millis(90);
/// 停顿重排换了首选时,候选列表轻微变暗再亮起的时长。
const PROMOTE_FLASH: Duration = Duration::from_millis(160);
/// 展开/收起的形变时长;弹簧在 300ms 内收敛,与 Apple 的 smooth 预设同量级。
const EXPAND_MS: Duration = Duration::from_millis(300);

/// 展开/收起的形变:同一块玻璃的窗口矩形从旧态长到新态,内容交叉淡入。
#[derive(Clone, Copy)]
struct ExpandFx {
    from: (i32, i32, i32, i32),
    to: (i32, i32, i32, i32),
    start: Instant,
    end: Instant,
    /// 候选窗在光标上方:底边贴住光标侧不动,向上生长。
    up: bool,
    /// 目标态是展开(箭头由 ⌄ 转到 ⌃)。
    expanding: bool,
}

impl ExpandFx {
    /// 弹簧进度;「减少动态效果」由调用方直接跳过,不走到这里。
    fn progress(&self, now: Instant) -> f32 {
        let total = self.end.saturating_duration_since(self.start).as_secs_f32();
        if total <= 0.0 {
            return 1.0;
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / total;
        spring_out(t.clamp(0.0, 1.0))
    }

    /// 当前窗口矩形;高度按进度插值,贴住光标的那条边保持不动。
    fn rect(&self, now: Instant) -> (i32, i32, i32, i32) {
        let p = self.progress(now);
        let lerp = |a: i32, b: i32| (a as f32 + (b - a) as f32 * p).round() as i32;
        let h = lerp(self.from.3, self.to.3).max(1);
        let x = lerp(self.from.0, self.to.0);
        let w = lerp(self.from.2, self.to.2).max(1);
        let y = if self.up {
            self.to.1 + self.to.3 - h
        } else {
            self.to.1
        };
        (x, y, w, h)
    }
}

/// 临界阻尼弹簧:起步快、收尾稳,没有可见回弹;Apple 的 smooth 预设
/// 就是这个走向,窗口形变不需要弹跳。归一化到端点 1,终帧不会补跳。
fn spring_out(t: f32) -> f32 {
    const K: f32 = 6.0;
    let curve = |t: f32| {
        let kt = K * t;
        1.0 - (1.0 + kt) * (-kt).exp()
    };
    curve(t) / curve(1.0)
}

/// 候选窗的动画与交互状态;全部归界面线程所有。
#[derive(Default)]
pub(super) struct Fx {
    /// 淡出截止时刻;期间沿用最后一份内容并让鼠标穿透。
    hide: Option<Instant>,
    /// 最后一份组合内容;淡出期间冻结,平时跟最新一帧。
    last: Option<SessionView>,
    /// 正在进行的翻页滑动:方向与截止时刻。
    slide: Option<(f32, Instant)>,
    /// 上一帧的页码,用来判断翻页方向。
    page: Option<usize>,
    /// 换首选提示的截止时刻。
    flash: Option<Instant>,
    /// 展开/收起的窗口形变;静止时为 None。
    expand: Option<ExpandFx>,
    /// 上一次看到的换首选次数;变化即触发一次提示。
    promotions: u32,
    /// 本次组合里候选窗在光标上方还是下方;同一次组合内保持不变。
    above: Option<bool>,
    /// 系统「减少动态效果」;开启后全部动效走终态。
    reduce: bool,
    /// 页内悬停候选;鼠标移出或换页后由重绘修正。
    pub(super) hover: Option<usize>,
    /// 悬停的翻页箭头;单页时恒为 `None`。
    /// 悬停的 ⌄ 展开按钮。
    pub(super) chev: bool,
    /// 鼠标停在棋盘滚动条上;显示加宽态。
    pub(super) scroll_hover: bool,
    /// 拖动滚动条时鼠标相对滑块顶部的偏移(物理像素)。
    pub(super) scroll_drag: Option<f32>,
    /// 打开的右键菜单:(页内槽位, 菜单顶部 y, 行文本)。行文本变了菜单自动失效。
    pub(super) menu: Option<(usize, f32, String)>,
    /// 悬停的菜单项。
    pub(super) menu_hover: Option<usize>,
    /// 鼠标最后位置(客户区);WM_SETCURSOR 用它做命中,省一次坐标换算。
    pub(super) mouse: (f32, f32),
    /// 最近一帧的绘制输入;命中测试用它,保证所指即所画。
    pub(super) geometry: Option<Geom>,
    /// 本次组合的起点;外观扩展的时钟从这儿算。
    since: Option<Instant>,
    /// 静止位置(屏幕坐标);发布给扩展的矩形用它。
    rest: (i32, i32, i32, i32),
}

impl Fx {
    /// 回到空闲态;reduce 偏好保留。
    fn clear(&mut self) {
        *self = Fx {
            reduce: self.reduce,
            // 次数跨组合保留,重新出现时不被当成一次新的换首选。
            promotions: self.promotions,
            ..Fx::default()
        };
    }

    /// 淡出中:窗口沿用冻结内容并让鼠标穿透。
    pub(super) fn fading(&self) -> bool {
        self.hide.is_some()
    }

    /// 效果的动画时钟:本次组合开始后的秒数;「减少动态效果」时冻结为 0。
    fn clock(&self, now: Instant) -> f32 {
        if self.reduce {
            return 0.0;
        }
        self.since
            .map(|since| now.saturating_duration_since(since).as_secs_f32())
            .unwrap_or(0.0)
    }

    /// 入场/翻页进度:静止为 1,淡出时退回 0。
    fn anim(&self, now: Instant) -> f32 {
        if let Some(end) = self.hide {
            return 1.0 - ease_out_quint(progress(HIDE_FADE, end, now));
        }
        1.0
    }
}

/// 最近一帧的绘制输入快照;鼠标命中据此换算,不重算布局。
pub(super) struct Geom {
    pub(super) spec: PanelSpec,
    pub(super) metrics: Metrics,
    pub(super) menu_xy: Option<(f32, f32)>,
}

/// 窗口过程持有的上下文;窗口与界面线程同生命周期,进程退出一起回收。
pub(super) struct WinCtx {
    /// 上一次发布给原生扩展的候选窗状态;内容没变就不再发。
    sent: Option<CandidateState>,
    /// 候选窗窗口。
    pub(super) hwnd: HWND,
    pub(super) state: Arc<State>,
    pub(super) fx: Fx,
    /// 出帧管线;设备被摘掉时整个换成新的,所以是 Option。
    pipe: Option<Pipeline>,
    /// DirectWrite 文字引擎;布局与绘制共用同一份测宽。
    pub(super) text: Text,
    pub(super) cursor_arrow: Option<HCURSOR>,
    pub(super) cursor_hand: Option<HCURSOR>,
    /// 运行期扩展:后台 worker 跑 Wasm 组件,这里只收主题色补丁。
    /// 扩展宿主连接:外观提交与补丁取回都经它,从不等待宿主。
    script: Arc<lueying_server::ext_link::ExtLink>,
    script_accent: Option<u32>,
    /// 隐私字段(密码框):外观扩展开关与补丁停用。
    private: bool,
    /// 首帧真正呈现前窗口保持隐藏,避免从停靠点挪到光标处时闪一帧旧内容。
    visible: bool,
    /// 出帧路径彻底走不通:候选窗停用。
    dead: bool,
    /// 已落的背板材质:(期望态, 自绘背板, 亚克力是否启用);变化才调窗口属性。
    material: Option<(Material, bool, bool)>,
    /// E 路线活背板:逐帧重捕,淡出期间沿用。
    backdrop: Option<glass::Backdrop>,
    /// 折射透镜:位移图按尺寸缓存;管线重建时随设备作废。
    lens: Option<glass::Lens>,
}

/// 候选窗:窗口、出帧管线与文字引擎。窗口过程经 `GWLP_USERDATA` 反查
/// 上下文,上下文泄漏到进程结束,与界面线程同寿命。
pub(crate) struct Candidate {
    ctx: *mut WinCtx,
    /// 上一次摆位用的版本号;版本变了才重新摆位。
    last_version: u64,
}

/// 建候选窗。建窗或文字引擎起不来只写日志并返回 `None`,输入法照常工作;
/// 出帧管线起不来则窗口停用,仍返回候选窗但它一直停靠在屏幕外。
pub(super) fn init(state: &Arc<State>) -> Option<Candidate> {
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            println!("D2D 候选窗建窗失败: {e}");
            return None;
        }
    };
    let pipe = match Pipeline::new(hwnd) {
        Ok(pipe) => Some(pipe),
        Err(e) => {
            println!("D2D 候选窗管线不可用: {e};候选窗停用");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            None
        }
    };
    let text = match Text::new() {
        Ok(text) => text,
        Err(e) => {
            println!("D2D 文字引擎起不来: {e}");
            return None;
        }
    };
    let (cursor_arrow, cursor_hand) = unsafe {
        (
            LoadCursorW(None, IDC_ARROW).ok(),
            LoadCursorW(None, IDC_HAND).ok(),
        )
    };
    let dead = pipe.is_none();
    let ctx: *mut WinCtx = Box::leak(Box::new(WinCtx {
        sent: None,
        hwnd,
        state: Arc::clone(state),
        fx: Fx::default(),
        pipe,
        text,
        cursor_arrow,
        cursor_hand,
        script: Arc::clone(&state.ext),
        script_accent: None,
        private: false,
        visible: false,
        dead,
        material: None,
        backdrop: None,
        lens: None,
    }));
    unsafe {
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx as isize);
    }
    Some(Candidate {
        ctx,
        last_version: 0,
    })
}

impl Candidate {
    /// 一轮:脚本补丁到了就应用;版本变了就摆位;动画进行中就推一帧。
    pub(super) fn tick(&mut self, state: &State) {
        let ctx = unsafe { &mut *self.ctx };
        if let Some((seq, patch)) = ctx.script.take_patch() {
            let redraw_now = state.version.load(Ordering::Acquire) == self.last_version;
            apply_script(ctx, seq, patch, redraw_now);
        }
        let version = state.version.load(Ordering::Acquire);
        if version != self.last_version {
            self.last_version = version;
            ctx.fx.reduce = reduce_motion();
            place(ctx);
        }
        if busy_animating(ctx) {
            animate(ctx);
        } else if live_glass(ctx) {
            redraw(ctx);
        }
    }

    /// 动画进行中按动画帧推进;活背板可见时按 LIVE_TICK 推帧;其余静止。
    pub(super) fn timeout(&self) -> Option<Duration> {
        let ctx = unsafe { &*self.ctx };
        if ctx.fx.expand.is_some() {
            Some(EXPAND_TICK)
        } else if busy_animating(ctx) {
            Some(ANIM_TICK)
        } else if live_glass(ctx) {
            Some(LIVE_TICK)
        } else {
            None
        }
    }
}

/// 淡出/滑动/换首选/展开形变是否还在推进。
fn busy_animating(ctx: &WinCtx) -> bool {
    let fx = &ctx.fx;
    fx.hide.is_some() || fx.slide.is_some() || fx.flash.is_some() || fx.expand.is_some()
}

/// 活背板要跟随背景变动:可见、非淡出、玻璃链在,就持续出帧重捕。
fn live_glass(ctx: &WinCtx) -> bool {
    ctx.visible && !ctx.fx.fading() && ctx.lens.is_some()
}

/// 组合变化后重新摆位并出帧;无内容时淡出或停靠。
fn place(ctx: &mut WinCtx) {
    if ctx.dead {
        // 出帧路径没了,窗口一直停在屏幕外,不再参与摆位与绘制。
        park(ctx);
        return;
    }
    match place::snapshot(&ctx.state) {
        None => {
            // 组合结束:reduce 直接停靠;否则窗口留在原地淡出,到期由动画帧停靠。
            if ctx.fx.reduce || ctx.fx.last.is_none() {
                ctx.fx.clear();
                park(ctx);
                return;
            }
            if ctx.fx.hide.is_none() {
                ctx.fx.hide = Some(Instant::now() + HIDE_FADE);
                ctx.fx.slide = None;
                ctx.fx.expand = None;
                ctx.fx.menu = None;
                ctx.fx.menu_hover = None;
                ctx.fx.hover = None;
                ctx.fx.chev = false;
                style_click_through(ctx.hwnd, true);
            }
            publish_state(ctx, None);
        }
        Some(snap) => {
            ctx.private = snap.private;
            let view = &snap.view;
            let fresh = ctx.fx.last.is_none() || ctx.fx.hide.is_some();
            if fresh {
                // 新一次组合重新判断候选窗朝向;候选行数变化时不会上下翻跳。
                ctx.fx.above = None;
                ctx.fx.slide = None;
                ctx.fx.expand = None;
                ctx.fx.page = None;
                ctx.fx.menu = None;
                ctx.fx.menu_hover = None;
                ctx.fx.hover = None;
                ctx.fx.chev = false;
                // 新组合:外观扩展的时钟从现在起算。
                ctx.fx.since = Some(Instant::now());
            }
            ctx.fx.hide = None;
            style_click_through(ctx.hwnd, false);
            // 只有固定条翻页才滑动;展开棋盘里挪高亮不横向跳。
            if !ctx.fx.reduce
                && effects_enabled(&ctx.state)
                && !view.expanded
                && ctx.fx.page.is_some_and(|page| page != view.page)
            {
                let previous = ctx.fx.page.unwrap_or(view.page);
                let direction = if view.page > previous { 1.0 } else { -1.0 };
                ctx.fx.slide = Some((direction, Instant::now() + PAGE_SLIDE));
                ctx.fx.hover = None;
                ctx.fx.chev = false;
            }
            ctx.fx.page = Some(view.page);
            if view.promotions != ctx.fx.promotions {
                ctx.fx.promotions = view.promotions;
                if !ctx.fx.reduce && effects_enabled(&ctx.state) {
                    ctx.fx.flash = Some(Instant::now() + PROMOTE_FLASH);
                }
            }
            let rect = caret_rect(ctx, view);
            if rect.2 == 0 {
                // 独占全屏或拿不到光标信息:停靠窗口,并清掉冻结内容,
                // 下一次组合按新组合出现,不会接着上一次的状态。
                ctx.fx.hide = None;
                ctx.fx.slide = None;
                ctx.fx.expand = None;
                ctx.fx.last = None;
                park(ctx);
                return;
            }
            let now = Instant::now();
            let toggled = !fresh
                && ctx
                    .fx
                    .last
                    .as_ref()
                    .is_some_and(|last| last.expanded != view.expanded);
            if toggled && !ctx.fx.reduce && effects_enabled(&ctx.state) {
                // 从当前窗口矩形(形变中就是当前帧)长到目标矩形。
                let from = ctx.fx.expand.map(|a| a.rect(now)).unwrap_or(ctx.fx.rest);
                let up = ctx.fx.above == Some(true);
                // 翻边(从光标下方跳到上方)不做高度形变:那会读成窗口瞬移。
                let same_side = if up {
                    from.1 + from.3 == rect.1 + rect.3
                } else {
                    from.1 == rect.1
                };
                ctx.fx.expand = (from.2 > 0 && from.3 > 0 && same_side).then(|| ExpandFx {
                    from,
                    to: rect,
                    start: now,
                    end: now + EXPAND_MS,
                    up,
                    expanding: view.expanded,
                });
                // 槽位重排后旧的悬停坐标已失效;箭头在棋盘里换了格子。
                ctx.fx.hover = None;
                ctx.fx.chev = false;
            } else if let Some(mut anim) = ctx.fx.expand {
                // 形变中内容变了:目标矩形跟上,动画继续走完。
                anim.to = rect;
                anim.up = ctx.fx.above == Some(true);
                ctx.fx.expand = Some(anim);
            }
            ctx.fx.rest = rect;
            let laid = ctx.fx.expand.map(|a| a.rect(now)).unwrap_or(rect);
            if let Err(e) = layout_window(ctx, laid) {
                println!("D2D 候选窗调整失败: {e}");
                return;
            }
            // 先落内容再出帧;淡出期间用的就是这份冻结内容。
            ctx.fx.last = Some(view.clone());
            submit_scene(ctx, view, snap.private);
            draw_view(ctx, view);
            publish_state(ctx, Some(view));
        }
    }
}

/// 把当前面板状态交给脚本线程;补丁晚一帧到,不影响这一帧。
fn submit_scene(ctx: &WinCtx, view: &SessionView, private: bool) {
    let now = Instant::now();
    let (_, _, accent) = place::ui_options(&ctx.state);
    let appearance = lueying_server::theme::Appearance::of_system();
    let colors = lueying_server::theme::colors_with(appearance, accent);
    let page_size = view.page_size.max(1);
    let page = panel::page_bounds(view.page, page_size, view.candidates.len());
    let candidates = page.len();
    let selected = if candidates == 0 {
        0
    } else {
        view.highlight
            .saturating_sub(page.start)
            .min(candidates - 1)
    };
    let scene = SceneIn {
        composing: true,
        page: (view.page + 1) as u32,
        pages: view.candidates.len().div_ceil(page_size).max(1) as u32,
        candidates: candidates as u32,
        selected: selected as u32,
        accent: colors.accent,
        dark: matches!(appearance, lueying_server::theme::Appearance::Dark),
        high_contrast: matches!(appearance, lueying_server::theme::Appearance::HighContrast),
        reduce_motion: ctx.fx.reduce,
        time: ctx.fx.clock(now),
        anim: ctx.fx.anim(now),
        private,
        ..SceneIn::default()
    };
    ctx.script.submit_scene(scene);
}

/// 应用脚本补丁;只有主题色还有效,内容没变就不重绘。
fn apply_script(ctx: &mut WinCtx, seq: u64, patch: Option<ScenePatch>, redraw_now: bool) {
    // 旧外观补丁晚到:新一帧或隐私字段已经提交,直接丢弃。
    if seq < ctx.script.scene_seq() {
        return;
    }
    let accent = patch.and_then(|patch| patch.accent);
    if accent == ctx.script_accent {
        return;
    }
    ctx.script_accent = accent;
    if redraw_now {
        redraw(ctx);
    }
}

/// 光标位置算出的候选窗矩形(x, y, 宽, 高);宽为 0 表示这次不弹。
fn caret_rect(ctx: &mut WinCtx, view: &SessionView) -> (i32, i32, i32, i32) {
    let (layout, ui_font, _) = place::ui_options(&ctx.state);
    let host_pid = (ctx.state.active.load(Ordering::Relaxed) >> 32) as u32;
    let Some((l, t, r, b)) = view.caret else {
        ctx.fx.above = None;
        ctx.state.candidate_above.store(false, Ordering::Relaxed);
        return (0, 0, 0, 0);
    };
    let left = l.min(r);
    // 按光标所在显示器算 DPI;停靠屏的缩放与本屏无关。
    let scale = place::scale_at(left, b);
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let mut spec = place::spec_of(view, layout, &metrics, &ctx.text).0;
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = place::measure_width(view, layout, &metrics, &ctx.text);
    let mut w = panel::panel_width(&spec, &metrics, &ctx.text) as i32;
    let h = panel::panel_height(&spec, &metrics, &ctx.text) as i32;
    let work = place::work_area_at(left, b);
    w = w.min((work.right - work.left).max(1));
    // 同一次组合内优先留在已选的一侧;只有这一侧放不下才翻边。
    // 副屏可以位于主屏上方,`top` 是负数,不能直接 `.max(0)`。
    let gap = (CARET_GAP * scale) as i32;
    let below = b + gap;
    let over = t - h - gap;
    let fits_below = below + h <= work.bottom;
    let fits_above = over >= work.top;
    let prefer_above = match ctx.fx.above {
        Some(previous) if previous && fits_above => true,
        Some(previous) if !previous && fits_below => false,
        _ => !fits_below && fits_above,
    };
    ctx.fx.above = Some(prefer_above);
    // 徽标据此躲到候选窗的另一侧,两者不互相遮挡。
    ctx.state
        .candidate_above
        .store(prefer_above, Ordering::Relaxed);
    let y =
        (if prefer_above { over } else { below }).clamp(work.top, (work.bottom - h).max(work.top));
    let x = left.clamp(work.left, (work.right - w).max(work.left));
    // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
    let hidden = place::foreground_fullscreen_at(left, b, host_pid);
    let w = if hidden { 0 } else { w };
    (x, y, w, h)
}

/// 摆位并把表面尺寸对上。
fn layout_window(ctx: &mut WinCtx, rect: (i32, i32, i32, i32)) -> Result<(), String> {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return Err("候选窗尺寸为空".to_string());
    }
    unsafe {
        let _ = SetWindowPos(ctx.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    }
    let Some(pipe) = ctx.pipe.as_mut() else {
        return Err("出帧管线不可用".to_string());
    };
    pipe.rebuild_surfaces(w as u32, h as u32)
}

/// 动画帧:淡出/滑动/换首选提示推进中重绘;淡出到期停靠,其余到期走终态。
fn animate(ctx: &mut WinCtx) {
    if ctx.dead {
        park(ctx);
        return;
    }
    let now = Instant::now();
    if ctx.fx.hide.is_some_and(|end| now >= end) {
        ctx.fx.clear();
        style_click_through(ctx.hwnd, false);
        park(ctx);
        return;
    }
    // 展开形变:窗口高度逐帧弹簧推进;到期落到精确目标矩形。
    let mut moved = false;
    if let Some(anim) = ctx.fx.expand {
        let rect = if now >= anim.end {
            ctx.fx.expand = None;
            anim.to
        } else {
            anim.rect(now)
        };
        if let Err(e) = layout_window(ctx, rect) {
            println!("D2D 候选窗形变失败: {e}");
            return;
        }
        moved = true;
    }
    // 到期的动画清掉;终态与上一帧在视觉上已重合,下轮不再进动画分支。
    if ctx.fx.slide.is_some_and(|(_, end)| now >= end) {
        ctx.fx.slide = None;
    }
    if ctx.fx.flash.is_some_and(|end| now >= end) {
        ctx.fx.flash = None;
    }
    let animating =
        ctx.fx.hide.is_some() || ctx.fx.slide.is_some() || ctx.fx.flash.is_some() || moved;
    if let Some(view) = animating.then(|| ctx.fx.last.clone()).flatten() {
        draw_view(ctx, &view);
    }
}

/// 出一帧:淡出透明度、列表动效、悬停与菜单,全部换算后交给管线。
fn draw_view(ctx: &mut WinCtx, view: &SessionView) {
    if ctx.dead {
        return;
    }
    // 设备被系统摘掉(驱动重启、GPU 挂起)后整条管线重建,窗口不用换。
    if let Some(pipe) = ctx.pipe.as_ref()
        && pipe.device_removed()
    {
        println!("D2D 候选窗:设备被摘掉,就地重建管线");
        ctx.pipe = None;
        match Pipeline::new(ctx.hwnd) {
            Ok(pipe) => {
                ctx.pipe = Some(pipe);
                ctx.lens = None;
                ctx.visible = false;
                // 新表面尺寸跟窗口走,重摆一次把表面上对。
                let rect = caret_rect(ctx, view);
                if let Err(e) = layout_window(ctx, rect) {
                    println!("D2D 候选窗调整失败: {e}");
                    return;
                }
            }
            Err(e) => {
                ctx.dead = true;
                println!("D2D 候选窗:管线重建失败({e}),候选窗停用");
                return;
            }
        }
    }
    let (layout, ui_font, accent) = place::ui_options(&ctx.state);
    let appearance = lueying_server::theme::Appearance::of_system();
    // 高对比度由系统接管外观,外观扩展整个关掉,免得压过系统配色。
    let high_contrast = matches!(appearance, lueying_server::theme::Appearance::HighContrast);
    // 脚本主题色只换主色与选中底色;高对比度仍用系统色。
    let colors = match ctx.script_accent {
        Some(rgb) if !high_contrast => lueying_server::theme::colors_with_accent(appearance, rgb),
        _ => lueying_server::theme::colors_with(appearance, accent),
    };
    let palette = lueying_server::theme::palette(colors);
    // 摆位时已把窗口放到光标屏;动效重绘沿用同一份缩放。
    let scale = match view.caret {
        Some((l, _, r, b)) => place::scale_at(l.min(r), b),
        None => 1.0,
    };
    // 材质背板:毛玻璃由 DWM 出底色,自绘只铺半透明内容层;系统关闭
    // 「透明效果」、高对比度或落属性失败时回实底主题。
    let wanted = Material {
        on: !high_contrast && material::transparency_enabled(),
        tint: colors.bg,
        dark: matches!(appearance, lueying_server::theme::Appearance::Dark),
    };
    // E 路线活背板:逐帧捕窗口区+外扩,背景变动实时跟随;窗口带排除捕获,
    // 捕不到自己,无需藏窗。淡出期间沿用旧背板保持观感稳定。
    // 捕获失败回退 DWM 亚克力。
    if wanted.on && !ctx.fx.fading() {
        ctx.backdrop = ctx
            .pipe
            .as_ref()
            .and_then(|pipe| glass::capture(&pipe.context(), ctx.hwnd, scale));
    }
    // 透镜折射:背板裁到窗口区,bezel 带里弯折;位移图按尺寸缓存。
    // 捕获区即窗口区+外扩,故 rel 恒为 (m, m),输出坐标相对窗口。
    let m = glass::margin(scale) as f32;
    let lens_image = if wanted.on {
        ctx.backdrop.as_ref().and_then(|b| {
            let mut wr = RECT::default();
            unsafe {
                let _ = GetWindowRect(ctx.hwnd, &mut wr);
            }
            let (w, h) = ((wr.right - wr.left) as u32, (wr.bottom - wr.top) as u32);
            if ctx.lens.as_ref().is_none_or(|l| !l.matches(w, h, scale)) {
                ctx.lens = ctx
                    .pipe
                    .as_ref()
                    .and_then(|p| glass::lens(&p.context(), w, h, scale, PANEL_RADIUS * scale));
            }
            let dc = ctx.pipe.as_ref()?.context();
            ctx.lens.as_ref()?.image(&dc, &b.image, (m, m), m)
        })
    } else {
        None
    };
    let backdrop = lens_image.as_ref().map(|image| (image, (-m, -m)));
    // 透镜链失败(设备/尺寸异常)时回退 DWM 亚克力,绝不露原始桌面。
    let self_blur = lens_image.is_some();
    let applied = match ctx.material {
        Some((cached, mode, applied)) if cached == wanted && mode == self_blur => applied,
        _ => {
            let applied = material::apply(ctx.hwnd, wanted, self_blur);
            ctx.material = Some((wanted, self_blur, applied));
            applied
        }
    };
    let glass = self_blur || applied;
    // 候选条玻璃的身份固定为深色烟熏层,墨色与主题无关:始终浅墨。
    // 同一块玻璃在白底换深字、黑底换浅字,看起来就是两套候选窗。
    let palette = if glass {
        let glass_colors = match ctx.script_accent {
            Some(rgb) if !high_contrast => lueying_server::theme::colors_with_accent(
                lueying_server::theme::Appearance::Dark,
                rgb,
            ),
            _ => {
                lueying_server::theme::colors_with(lueying_server::theme::Appearance::Dark, accent)
            }
        };
        glass_ink(lueying_server::theme::palette(glass_colors))
    } else {
        palette
    };
    let metrics = Metrics::scaled(ui_font.factor() * scale);
    let (mut spec, fit) = place::spec_of(view, layout, &metrics, &ctx.text);
    spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
    spec.width = place::measure_width(view, layout, &metrics, &ctx.text);
    // 条数自适应:量出的可见条数喂回引擎当页大小;应答落地后下一帧对齐。
    if fit != view.page_size {
        let session = ctx.state.active.load(Ordering::Relaxed);
        let _ = crate::imp::handle(
            &ctx.state,
            lueying_protocol::Request::SetPageSize { session, size: fit },
        );
    }
    let now = Instant::now();
    // 展开/收起:另一态的面板输入 + 弹簧进度。管线把新旧两套内容压在
    // 同一块玻璃上交叉淡入,玻璃本身只画一层。
    let window_h = {
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(ctx.hwnd, &mut wr);
        }
        (wr.bottom - wr.top).max(1) as f32
    };
    let morph = ctx.fx.expand.map(|anim| {
        let mut other = view.clone();
        other.expanded = !view.expanded;
        let (mut other_spec, _) = place::spec_of(&other, layout, &metrics, &ctx.text);
        other_spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
        other_spec.width = place::measure_width(&other, layout, &metrics, &ctx.text);
        (
            other_spec,
            anim.progress(now),
            window_h,
            anim.up,
            anim.expanding,
        )
    });
    // 出现直接就位,透明度恒为 1;淡出仍按旧节奏渐隐。
    let opacity = if let Some(end) = ctx.fx.hide {
        (1.0 - ease_out_quint(progress(HIDE_FADE, end, now))).max(0.0)
    } else {
        1.0
    };
    let list = match (ctx.fx.slide, ctx.fx.flash) {
        (Some((direction, end)), _) if now < end => {
            let delta = ease_out_quint(progress(PAGE_SLIDE, end, now));
            panel::ListFx {
                dx: direction * 10.0 * metrics.scale() * (1.0 - delta),
                opacity: 0.55 + 0.45 * delta,
            }
        }
        (_, Some(end)) if now < end => {
            // 换首选:列表只轻微变暗再亮起,不横向移动。
            let delta = ease_out_quint(progress(PROMOTE_FLASH, end, now));
            panel::ListFx {
                dx: 0.0,
                opacity: 0.72 + 0.28 * delta,
            }
        }
        _ => panel::ListFx::NEUTRAL,
    };
    // 菜单只挂在内容未变的行上;翻页或继续打字后行文本对不上,菜单自动失效。
    // 展开形变中菜单暂时收起,形状稳定后再按新几何重锚。
    let menu = if morph.is_some() {
        None
    } else {
        ctx.fx.menu.as_ref().and_then(|(slot, _, text)| {
            spec.items
                .get(*slot)
                .is_some_and(|item| &item.text == text)
                .then(|| menu_origin(*slot, &spec, &metrics, &ctx.text))
        })
    };
    if menu.is_none() && morph.is_none() {
        ctx.fx.menu = None;
        ctx.fx.menu_hover = None;
    }
    // 形变中几何每帧都在变,命中测试暂停,避免点到还没长出来的格子。
    ctx.fx.geometry = if morph.is_some() {
        None
    } else {
        Some(Geom {
            spec: spec.clone(),
            metrics,
            menu_xy: menu,
        })
    };
    let frame = Frame {
        spec: &spec,
        palette: &palette,
        metrics,
        glass,
        backdrop,
        opacity,
        list,
        hover: ctx.fx.hover,
        chev: ctx.fx.chev,
        scroll_hover: ctx.fx.scroll_hover,
        morph: morph
            .as_ref()
            .map(|(spec, progress, height, up, expanding)| Morph {
                from: spec,
                progress: *progress,
                height: *height,
                up: *up,
                expanding: *expanding,
            }),
        menu,
        menu_hover: ctx.fx.menu_hover,
    };
    let drawn = match ctx.pipe.as_mut() {
        Some(pipe) => pipe
            .paint_frame(&frame, &ctx.text)
            .and_then(|()| pipe.present()),
        None => Err("出帧管线不可用".to_string()),
    };
    if let Err(e) = drawn {
        println!("D2D 出帧失败,丢帧: {e}");
        if dump_on() {
            let _ = std::fs::write(
                std::env::temp_dir().join("lueying-glass-dbg.txt"),
                format!("drawn err: {e}"),
            );
        }
        return;
    }
    // 取证开关:实机截图因排除捕获拿不到候选窗,开关开启才读回帧纹理。
    if dump_on() {
        let luma = ctx.backdrop.as_ref().map(|b| b.luma);
        let _ = std::fs::write(
            std::env::temp_dir().join("lueying-glass-dbg.txt"),
            format!("drawn ok luma={luma:?} self_blur={self_blur}"),
        );
        if let Some(pipe) = ctx.pipe.as_ref() {
            pipe.dump_frame(&std::env::temp_dir().join("lueying-live-frame.bmp"));
        }
    }
    show(ctx);
    // 每帧都回到 topmost 队首:别的置顶窗(工具提示、其它输入法)不能把它压住。
    unsafe {
        let _ = SetWindowPos(
            ctx.hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// 鼠标/菜单变化后的重绘;淡出窗口不接鼠标,内容与位置都不动。
pub(super) fn redraw(ctx: &mut WinCtx) {
    if ctx.fx.fading() {
        return;
    }
    let Some(view) = ctx.fx.last.clone() else {
        return;
    };
    draw_view(ctx, &view);
}

/// 菜单锚点:锚在行下缘,贴近面板底部时翻到行上方。
/// 行位置取自 `item_rects`,横排换行时也不会锚错行。
pub(super) fn menu_origin(
    slot: usize,
    spec: &PanelSpec,
    metrics: &Metrics,
    measure: &dyn panel::Measure,
) -> (f32, f32) {
    let s = metrics.scale();
    let panel_h = panel::panel_height(spec, metrics, measure);
    let menu_h = panel::menu_height(metrics);
    let rects = panel::item_rects(spec, metrics, measure);
    // 菜单只画在面板内部:上界是首行行顶,锚不下就压着行画。
    let floor = rects.first().map_or(0.0, |first| first.top);
    let Some(rect) = rects.get(slot).copied() else {
        return (8.0 * s, floor);
    };
    let top = if rect.bottom + menu_h <= panel_h {
        rect.bottom
    } else {
        (rect.top - menu_h).max(floor)
    };
    (8.0 * s, top)
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ctx = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WinCtx;
    if ctx.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    match msg {
        // 回落宿主(任务栏搜索)切输入法时从宿主进程投递;UIPI 已放行。
        lueying_protocol::FALLBACK_ACTIVE_MESSAGE => {
            crate::imp::fallback::report_active(
                &unsafe { &*ctx }.state,
                lparam.0 as u32,
                wparam.0 != 0,
            );
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_CAPTURECHANGED
        | WM_RBUTTONDOWN | WM_MOUSEWHEEL | WM_SETCURSOR => unsafe {
            mouse::on_mouse(&mut *ctx, hwnd, msg, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 淡出期间点击穿透到宿主;实时组合才接鼠标。
fn style_click_through(hwnd: HWND, click_through: bool) {
    let old = ex_style(hwnd);
    let mut style = old;
    if click_through {
        style |= WS_EX_TRANSPARENT.0;
    } else {
        style &= !WS_EX_TRANSPARENT.0;
    }
    if style != old {
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
        }
    }
}

/// 窗口的扩展样式。
fn ex_style(hwnd: HWND) -> u32 {
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 }
}

/// 弹性动效开关;配置重载后立即生效。
fn effects_enabled(state: &State) -> bool {
    state
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .effects
}

/// 系统「减少动态效果」;开启后动效全部走终态。查询失败按可用处理。
pub(super) fn reduce_motion() -> bool {
    let mut enabled = BOOL::default();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut enabled as *mut BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .is_ok();
    ok && !enabled.as_bool()
}

/// 0..1 的动画进度;已过期钳到 1。
fn progress(total: Duration, end: Instant, now: Instant) -> f32 {
    let remain = end.checked_duration_since(now).unwrap_or_default();
    (1.0 - remain.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

/// ease_out_quint;窗口滑动与淡出动效共用这条曲线。
fn ease_out_quint(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(5)
}

// uiAccess 进程创建的窗口自动落进 ZBID_UIACCESS 层——它高于一切普通窗口,
// 也高于 shell 浮层(SearchHost 面板)。注意 ZBID 编号只是 ID:浮层的
// IMMERSIVE_SEARCH=13 编号虽大,高度却在 UIACCESS 之下,不要按编号比大小。

/// 无焦点置顶工具窗;样式在创建时给足,停靠点出生,避免 Alt+Tab 里露脸。
pub(super) fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        ensure_class(HINSTANCE(hmodule.0))?;
        let class = w!("lueying-d2d-candidate");
        let (park_x, park_y) = place::park_point();
        let hwnd = CreateWindowExW(
            // NOREDIRECTIONBITMAP:窗口自身不画,画面由 DComp 视觉树给出。
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP,
            class,
            w!("lueying-candidate"),
            // 先隐藏建在停靠点;首帧真正提交后再显示,避免闪一帧旧内容。
            WS_POPUP,
            park_x,
            park_y,
            360,
            76,
            None,
            None,
            Some(HINSTANCE(hmodule.0)),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        // 排除捕获:逐帧活背板捕不到自己,无需藏窗(参照 taskbar-liquid-glass)。
        // 代价:录屏/截图里候选窗不出现;与 Windhawk 玻璃 mod 同一取舍。
        let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        // 低完整性宿主(AppContainer)要能投递输入法状态消息。
        let _ = ChangeWindowMessageFilterEx(
            hwnd,
            lueying_protocol::FALLBACK_ACTIVE_MESSAGE,
            MSGFLT_ALLOW,
            None,
        );
        Ok(hwnd)
    }
}

/// 窗口类进程内只注册一次(自检与候选窗可能先后建窗)。
fn ensure_class(hinstance: HINSTANCE) -> Result<(), String> {
    static ONCE: Once = Once::new();
    let mut result = Ok(());
    ONCE.call_once(|| unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            hCursor: match LoadCursorW(None, IDC_ARROW) {
                Ok(cursor) => cursor,
                Err(e) => {
                    result = Err(format!("LoadCursorW: {e}"));
                    return;
                }
            },
            lpszClassName: w!("lueying-d2d-candidate"),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            result = Err("RegisterClassW 失败".into());
        }
    });
    result
}

/// 取证开关:temp 下存在 lueying-dump-on 才读回/落盘。用文件不用环境变量:
/// 服务可能被 ext-host 无环境变量拉起,文件开关对任何实例都生效。
pub(super) fn dump_on() -> bool {
    std::env::temp_dir().join("lueying-dump-on").exists()
}

/// 亮背板上的玻璃用深色墨:只换内容令牌,菜单与实底仍跟主题。
/// 固定深色玻璃的墨色修正。玻璃压在亮背板上会抬到 180,次级/序号墨
/// 必须亮过它,否则白底上序号和箭头翻成深色,和 macOS 候选条相反。
pub(super) fn glass_ink(mut p: Palette) -> Palette {
    // 暖墨玻璃配暖白墨;深色/白底都靠同一组暖色立住。
    p.text = 0xece7df;
    p.text_secondary = 0xe3ddd2;
    p.text_tertiary = 0xd4cec3;
    p.border = 0x4a4238;
    p.hover = 0x3a332a;
    // 选中槽是亮面黄釉胶囊:帝王黄渐变、深墨字、深琥珀序号。
    p.accent = 0xd9a92e;
    p.on_accent = 0x2b2110;
    p.selected_bg = 0xe7c24a;
    p.selected_text = 0x2b2110;
    p.accent_text = 0x7a5608;
    p
}

/// 首帧真正提交后再显示;无焦点显示,不抢宿主焦点。
fn show(ctx: &mut WinCtx) {
    if ctx.visible {
        return;
    }
    unsafe {
        let _ = ShowWindow(ctx.hwnd, SW_SHOWNOACTIVATE);
    }
    ctx.visible = true;
}

/// 挪到虚拟桌面之外并隐藏;停靠点按当前虚拟桌面算。
fn park(ctx: &mut WinCtx) {
    if ctx.visible {
        unsafe {
            let _ = ShowWindow(ctx.hwnd, SW_HIDE);
        }
        ctx.visible = false;
    }
    let (x, y) = place::park_point();
    place::park(ctx.hwnd, x, y);
    publish_state(ctx, None);
}

/// 把候选窗的当前状态发布给订阅的原生扩展;内容没变就不发。
/// 私密字段的文字在这里就留空,扩展拿不到。
fn publish_state(ctx: &mut WinCtx, view: Option<&SessionView>) {
    let (x, y, w, h) = ctx.fx.rest;
    let private = ctx.private;
    let mut state = CandidateState {
        visible: ctx.visible,
        composing: view.is_some(),
        rect: [x, y, w, h],
        private,
        ..CandidateState::default()
    };
    if let Some(view) = view.filter(|_| !private) {
        state.raw.clone_from(&view.raw);
        state.page = view.page;
        state.page_size = view.page_size;
        state.highlight = view.highlight;
        state.candidates.clone_from(&view.candidates);
    }
    if ctx.sent.as_ref() == Some(&state) {
        return;
    }
    ctx.state.ext.publish_candidates(state.clone());
    ctx.sent = Some(state);
}
