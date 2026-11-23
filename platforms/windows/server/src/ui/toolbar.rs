//! 输入法工具栏:一排常用按钮的悬浮条,Rune overlay 承载。
//!
//! 与候选窗、徽标同一 App、同一条线程;模式状态字(中英/全半/标点/简繁)
//! 与共用材质(玻璃/黑瓷、磨砂、变体、强调色)在每一轮 poll 里从同一份
//! 配置快照对读,变化才重跑 view。位置与倍率持久化在配置里,拖动结束
//! 写回。按钮动作复用服务端已有的请求处理。位置、大小与按钮的模型在
//! `lueying_render::toolbar`,这里只负责窗口与交互。
//!
//! 玻璃与候选窗共用 `ui::material`:抓取条体外的桌面像素,过同一套
//! 折射/色散光学,铺在窗口圆角裁切层下。背板抓取要求窗口的捕获排除
//! 已被系统接受;不明或失败时宁可不采,绝不把工具栏自己录进画面。

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use lueying_protocol::Request;
use lueying_render::panel::{Palette, Rect};
use lueying_render::toolbar::{self, Action, Button, Status, Toolbar};
use lueying_server::config::{self, UiMaterial};
use lueying_server::theme;
use rune::tok::motion;
use rune::{
    Align, Cursor, Cx, Drag, El, KeyPolicy, OverlayHandle, OverlayInput, OverlayOptions,
    OverlaySpec, OverlaySupport, Phase, Pixels, SurfaceFit, TextStyle, Tx, el, text,
};
use windows::Win32::Foundation::RECT;

use crate::imp::{State, handle};
use crate::liquid;
use crate::ui::{material, place};

/// 常驻的 overlay 句柄;别的线程靠它叫醒本线程。
static HANDLE: OnceLock<OverlayHandle<BarHud>> = OnceLock::new();

/// 活玻璃的背板重捕节奏(~30fps):背景变动跟随,再高肉眼无感只费电。
const LIVE_TICK: Duration = Duration::from_millis(33);
/// 釉光环境窄带的重采节奏;位置变化会提前重采。
const AMBIENT_TICK: Duration = Duration::from_millis(500);
/// 捕获排除失败后的低频复查;拿出来 Backdrop 只会白费 BitBlt。
const CAPTURE_RETRY: Duration = Duration::from_secs(1);

/// 月相弹簧:0=弯月,1=满月;全宽翻转时以位置/速度接力,物理参数
/// 只有这一份。
const MOON_SPRING: rune::Spring = rune::Spring::new(0.28, 0.12);

/// 一轮 poll 从一次短锁里取出的配置快照;材质、外观与状态字共用。
struct ConfigSnapshot {
    scale: f32,
    material: UiMaterial,
    effects: bool,
    frost: f32,
    variant: String,
    band: f32,
    accent: usize,
    punct: bool,
    traditional: bool,
}

impl ConfigSnapshot {
    fn read(state: &State) -> Self {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        Self {
            scale: toolbar::clamp_scale(cfg.toolbar_scale),
            material: cfg.ui_material,
            effects: cfg.effects,
            frost: cfg.ui_glass_frost.clamp(0.0, 1.0),
            variant: cfg.ui_glass_variant.clone(),
            band: cfg.ui_glass_band,
            accent: cfg.ui_accent,
            punct: cfg.chinese_punct,
            traditional: cfg.traditional,
        }
    }
}

/// 工具栏 overlay 的状态;Rune 每帧从它重建控件树。
pub(super) struct BarHud {
    state: Arc<State>,
    /// 工具栏模型:可见性、倍率与位置;拖动数学也在里面。
    bar: Toolbar,
    /// 上次见到的状态字;变了才重跑 view。
    status: Status,
    /// 月相 0=弯月 1=满月;形变期间由 poll 里的弹簧推进,cutout 按它重建。
    phase: f32,
    /// 月相弹簧当前段:起点位置、起点速度与起点时刻(帧钟秒)。
    /// 目标翻转时先按旧目标采样到 now,再以 (x,v,now) 接力。
    phase_from: (f32, f32, f64),
    /// 外观按 poll 取一次:系统主题切得很慢,每帧读注册表纯属浪费。
    appearance: theme::Appearance,
    /// 系统透明效果开关;关闭时玻璃退实底,黑瓷与显式光源照常。
    transparency: bool,
    /// 当前配置的材质缓存。
    material: UiMaterial,
    effects: bool,
    /// 系统「减少动态效果」;poll 约每秒复查,月相与控件树共用这一份,
    /// 不把 host 建窗时的旧快照当运行时开关。
    reduce: bool,
    frost: f32,
    glass: liquid::GlassVariant,
    glass_name: String,
    glass_band: f32,
    accent: usize,
    /// 活动会话的私密标记;采样前判定,转私密立刻作废已有背板。
    private: bool,
    hover: Option<Button>,
    /// overlay 已显示;隐藏后 show 只在 poll 里发生。
    shown: bool,
    drag_moved: bool,
    /// 最近一次摆位的屏幕物理矩形与显示器 DPI;背板与釉光共用同一份。
    placed: Option<((i32, i32, i32, i32), f32)>,
    /// 玻璃背板:流式像素通道,一次分配,逐帧 `set` 原地换桌面像素。
    backdrop: Option<Pixels>,
    backdrop_read: Option<(f64, (i32, i32, i32, i32))>,
    /// 上一次喂给光学的原始背板:静止时逐位一致,跳过重算与上传。
    backdrop_raw: Option<((u32, u32), Vec<u8>)>,
    /// 环境棱缘:同一张原始背板算出的直通 alpha 光带,随原图一起更新。
    rim: Option<Pixels>,
    /// 亚克力颗粒:条体物理尺寸的固定噪点图;随尺寸重生成。
    grain: Option<Pixels>,
    grain_size: Option<(u32, u32)>,
    /// 釉光的环境光:条体上下窄带的桌面实采均值;没有来源就不上光。
    glaze: Option<theme::Glaze>,
    glaze_read: Option<(f64, (i32, i32, i32, i32))>,
    /// 显示后是否已主动请求过捕获排除;默认 Unknown 时等窗口建好。
    exclusion_checked: bool,
}

impl BarHud {
    fn new(state: Arc<State>, bar: Toolbar) -> Self {
        let cfg = ConfigSnapshot::read(&state);
        let status = status_of(&state, &cfg);
        let glass = liquid::variant(&cfg.variant);
        let band = if cfg.band > 0.0 { cfg.band } else { glass.band };
        // 启动即落在当前月相,首帧不播形变。
        let phase = if status.fullwidth { 1.0 } else { 0.0 };
        Self {
            phase,
            phase_from: (phase, 0.0, 0.0),
            status,
            appearance: theme::Appearance::of_system(),
            transparency: theme::transparency_enabled(),
            material: cfg.material,
            effects: cfg.effects,
            reduce: !theme::client_area_animation(),
            frost: cfg.frost,
            glass,
            glass_name: cfg.variant,
            glass_band: band,
            accent: cfg.accent,
            private: false,
            state,
            bar,
            hover: None,
            shown: false,
            drag_moved: false,
            placed: None,
            backdrop: None,
            backdrop_read: None,
            backdrop_raw: None,
            rim: None,
            grain: None,
            grain_size: None,
            glaze: None,
            glaze_read: None,
            exclusion_checked: false,
        }
    }

    /// 按钮的动作。模式、全半角、标点复用服务端的请求处理;简繁写配置;
    /// 设置与收起直接处理。
    fn act(&mut self, button: Button) {
        match button.action() {
            Action::ToggleMode => {
                // active 为 0(焦点落在没有掠影会话的窗口)也照发:
                // 分发端没有会话就只翻全局位,按钮不能静默无效。
                let session = self.state.active.load(Ordering::Relaxed);
                let _ = handle(&self.state, Request::ToggleMode { session });
            }
            Action::ToggleFullWidth => {
                let session = self.state.active.load(Ordering::Relaxed);
                let _ = handle(&self.state, Request::ToggleFullwidth { session });
            }
            Action::TogglePunct => {
                let _ = handle(&self.state, Request::TogglePunct);
            }
            Action::ToggleScript => toggle_traditional(&self.state),
            Action::OpenSettings => open_settings(),
            Action::Hide => self.bar.visible = false,
        }
    }

    /// 清空玻璃背板/颗粒;位置、隐藏、转私密或换材质时用,防止旧截图
    /// 在新位置或新状态下继续显示。采样失败时只丢画面,保留读时刻
    /// 以便节流重试。
    fn drop_backdrop(&mut self) -> bool {
        let mut changed = false;
        changed |= self.backdrop.take().is_some();
        changed |= self.backdrop_raw.take().is_some();
        changed |= self.rim.take().is_some();
        changed |= self.grain.take().is_some();
        self.grain_size = None;
        changed
    }

    fn clear_backdrop(&mut self) -> bool {
        let changed = self.drop_backdrop();
        self.backdrop_read = None;
        changed
    }

    /// 连同环境釉光一起清空;釉光也来自旧位置/旧状态的窄带采样。
    fn clear_capture(&mut self) -> bool {
        let mut changed = self.clear_backdrop();
        changed |= self.glaze.take().is_some();
        self.glaze_read = None;
        changed
    }
}

/// 装进候选窗 App:第三个 overlay,隐藏期不占窗口与表面。
pub(super) fn register<T: 'static>(app: &mut rune::App<T>, state: Arc<State>) {
    let bar = saved_bar(&state);
    let spec = OverlaySpec::new("lueying-toolbar", bar.size(), toolbar_view)
        .options(OverlayOptions {
            // 不吃按键;只有标了 hit 的条体拦指针,圆角外原生穿透。
            keys: KeyPolicy::None,
            input: OverlayInput::Regions,
            // 与候选窗一致:系统支持的平台上让屏幕捕获看穿本体,
            // 背板才不会把工具栏自己录进去。
            exclude_from_capture: true,
            ..Default::default()
        })
        .release_gpu_when_hidden()
        .poll(toolbar_poll)
        // 事件循环一起就走一遍既有的 kick 路径(排空更新→首轮 poll→show);
        // 不然工具栏要等到第一次模式切换或设置保存才出现(升级装完服务
        // 重启后没有任何 kick,打字只叫醒候选窗)。
        .setup(|_: &mut BarHud, _| kick());
    let handle = app.overlay(spec, BarHud::new(state, bar));
    let _ = HANDLE.set(handle);
}

/// 配置或模式状态变了就叫醒工具栏;overlay 起来之前是空操作。
pub(super) fn kick() {
    if let Some(handle) = HANDLE.get() {
        handle.update(|_: &mut BarHud, _| {});
    }
}

/// 每帧开头跑一次:一次配置短锁、状态字、外观、摆位与材质采样。
/// 只有几何变化才重新 set_placement;材质刷新只换像素与控件树。
fn toolbar_poll(s: &mut BarHud, now: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    let cfg = ConfigSnapshot::read(&s.state);

    // 倍率是几何:变化时重新夹进当前工作区,摆位阶段再 set_placement。
    if (cfg.scale - s.bar.scale).abs() > f32::EPSILON {
        let work = place::work_area_at(s.bar.x as i32, s.bar.y as i32);
        s.bar = s.bar.with_scale(cfg.scale, to_rect(work));
        changed = true;
    }

    // 玻璃光学参数:变体/带宽变了要让下一轮捕获重算,旧背板也不能再露。
    let mut glass_dirty = false;
    if s.glass_name != cfg.variant {
        s.glass_name = cfg.variant.clone();
        s.glass = liquid::variant(&cfg.variant);
        glass_dirty = true;
    }
    let band = if cfg.band > 0.0 {
        cfg.band
    } else {
        s.glass.band
    };
    if (s.glass_band - band).abs() > f32::EPSILON {
        glass_dirty = true;
    }
    s.glass_band = band;
    if glass_dirty {
        s.clear_capture();
        changed = true;
    }

    if s.material != cfg.material
        || (s.frost - cfg.frost).abs() > f32::EPSILON
        || s.accent != cfg.accent
        || s.effects != cfg.effects
    {
        s.material = cfg.material;
        s.frost = cfg.frost;
        s.accent = cfg.accent;
        s.effects = cfg.effects;
        // 主题色/材质换色走官方主题变化弹簧;玻璃光学参数已在上方
        // 单独清背板。
        tx.color_motion(motion::CALM);
        changed = true;
    }

    // 私密判定在采样之前:活动会话转私密,已有背板与釉光立刻作废。
    let private = s
        .state
        .active_published()
        .is_some_and(|published| published.private);
    if private != s.private {
        s.private = private;
        changed = true;
    }

    // 减少动态效果来自主动运行时读取(约 1 秒缓存),与上面同一份
    // SPI;host 的建窗快照可能落后,不能作为唯一来源。
    let reduce = !theme::client_area_animation();
    if s.reduce != reduce {
        s.reduce = reduce;
        changed = true;
    }

    // 旧目标必须在覆盖状态之前取:全宽翻转的一瞬,先把旧 spring
    // 精确采样到当前 now,再以同一位置/速度接力到新目标。
    let old_target = if s.status.fullwidth { 1.0 } else { 0.0 };
    let status = status_of(&s.state, &cfg);
    if status != s.status {
        if status.fullwidth != s.status.fullwidth {
            if s.effects && !s.reduce {
                let (x, v) = MOON_SPRING.sample(
                    s.phase_from.0,
                    s.phase_from.1,
                    old_target,
                    (now - s.phase_from.2).max(0.0) as f32,
                );
                s.phase_from = (x, v, now);
                s.phase = x;
            } else {
                s.phase_from = (old_target, 0.0, now);
                s.phase = old_target;
            }
        }
        s.status = status;
        // 全/半切换是月亮的形变:下面的月相弹簧把暗面侵蚀到位。
        // 标点/简繁只换字,不重启正在进行的月相段。
        tx.animated();
        changed = true;
    }
    // 月相弹簧:0=弯月,1=满月;闭式弹簧按经过时间采样,形变期间
    // 每帧重采样并叫醒下一拍(shapes 示例的 poll 驱动同款)。是否在途
    // 看段起点是否已落在目标并清速,不用 phase != target 判断
    // (穿过目标但速度未消时仍是形变中)。系统关掉动画或用户关闭
    // 特效都直落并清速度。
    let target = if s.status.fullwidth { 1.0 } else { 0.0 };
    if !(s.effects && !s.reduce) {
        if s.phase != target || s.phase_from.0 != target || s.phase_from.1 != 0.0 {
            s.phase = target;
            s.phase_from = (target, 0.0, now);
            changed = true;
        }
    } else if s.phase_from.0 != target || s.phase_from.1 != 0.0 {
        let (p, v) = MOON_SPRING.sample(
            s.phase_from.0,
            s.phase_from.1,
            target,
            (now - s.phase_from.2).max(0.0) as f32,
        );
        // 位置与速度都到阈才落定;落定即写回终态,下一帧不再采样。
        if (p - target).abs() < 0.005 && v.abs() < 0.05 {
            s.phase = target;
            s.phase_from = (target, 0.0, now);
        } else {
            s.phase = p;
            tx.wake_after(Duration::ZERO);
        }
        changed = true;
    }

    let transparency = theme::transparency_enabled();
    if transparency != s.transparency {
        s.transparency = transparency;
        changed = true;
    }

    let appearance = theme::Appearance::of_system();
    if appearance != s.appearance {
        s.appearance = appearance;
        // 主题色与外观变化走官方的主题变化弹簧,颜色渐变过去。
        tx.color_motion(motion::CALM);
        changed = true;
    }

    let (rect, dpi) = bar_rect(s);
    if s.bar.visible {
        if !s.shown {
            // 显式要求捕获排除,并等窗口建好后的第一轮 poll 再吃支持状态。
            s.clear_capture();
            place_bar(s);
            if let Some(handle) = HANDLE.get() {
                handle.show();
            }
            s.shown = true;
            s.exclusion_checked = false;
            changed = true;
        } else if s.placed != Some((rect, dpi)) {
            // 位置或倍率变化:先丢掉旧位置的截图,再只摆这一次位。
            s.clear_capture();
            place_bar(s);
            changed = true;
        }
    } else if s.shown {
        if let Some(handle) = HANDLE.get() {
            handle.hide();
        }
        s.shown = false;
        s.exclusion_checked = false;
        s.clear_capture();
        changed = true;
    }

    let high_contrast = matches!(s.appearance, theme::Appearance::HighContrast);
    let Some((rect, dpi)) = s.placed else {
        return changed;
    };
    let shown = s.shown && s.bar.visible;

    // 玻璃背板:仅在实际采桌面时抓;捕获排除未明确可用就不录。
    let glass_live = shown
        && s.material.is_glass()
        && s.effects
        && s.transparency
        && !s.private
        && !high_contrast;
    if glass_live {
        let due = s
            .backdrop_read
            .is_none_or(|(last, area)| area != rect || now - last >= LIVE_TICK.as_secs_f64());
        if due {
            s.backdrop_read = Some((now, rect));
            let ready = capture_exclusion_ready(s);
            if ready {
                changed |= capture_backdrop(s, rect, dpi);
            } else {
                // 未明确可用时不录到自身;已有旧画面也一并撤掉。
                changed |= s.drop_backdrop();
            }
            // Unknown 仍在建窗,按活玻璃节奏复查;Failed/Unsupported
            // 只做低频偏好检查,不再每帧摸 BitBlt。
            let retry = if !ready
                && matches!(
                    HANDLE.get().map(|handle| handle.capture_exclusion()),
                    Some(OverlaySupport::Failed | OverlaySupport::Unsupported)
                ) {
                CAPTURE_RETRY
            } else {
                LIVE_TICK
            };
            tx.wake_after(retry);
        }
    } else {
        changed |= s.clear_backdrop();
    }

    // 窄带釉光只服务黑瓷:实际采样,采不到就是哑光。玻璃棱缘改由
    // 背板原图算出的环境棱缘流提供,不再额外采屏。
    let glaze_live = shown && s.effects && !s.private && !high_contrast && !s.material.is_glass();
    if glaze_live {
        let due = s
            .glaze_read
            .is_none_or(|(last, area)| area != rect || now - last >= AMBIENT_TICK.as_secs_f64());
        if due {
            s.glaze_read = Some((now, rect));
            let previous = s.glaze;
            s.glaze = material::sample_glaze(rect);
            changed |= previous != s.glaze;
        }
        if let Some((last, _)) = s.glaze_read {
            tx.wake_at(last + AMBIENT_TICK.as_secs_f64());
        }
    } else {
        s.glaze_read = None;
        changed |= s.glaze.take().is_some();
    }

    // 外观/透明/减动效偏好运行中可响应:显示期间每秒复查一次,
    // 不改现有帧;隐藏时不常驻轮询。
    if shown {
        tx.wake_after(Duration::from_secs(1));
    }

    changed
}

/// 捕获排除是否已被系统接受。Unknown 表示窗口还在建,等下一轮;
/// Failed/Unsupported 明确不可用时宁可不采样,绝不录到自身。
fn capture_exclusion_ready(s: &mut BarHud) -> bool {
    let Some(handle) = HANDLE.get() else {
        return false;
    };
    if !s.exclusion_checked {
        handle.set_exclude_from_capture(true);
        s.exclusion_checked = true;
    }
    material::desktop_capture_ready(Some(handle))
}

/// 条体的屏幕物理矩形与显示器 DPI;摆位、背板与釉光共用这一份。
fn bar_rect(s: &BarHud) -> ((i32, i32, i32, i32), f32) {
    let (w, h) = s.bar.size();
    let (x, y) = (s.bar.x as i32, s.bar.y as i32);
    let scale = place::scale_at(x, y);
    ((x, y, (w * scale) as i32, (h * scale) as i32), scale)
}

/// 摆位:模型的屏幕物理坐标换成 overlay placement(主屏本地逻辑坐标),
/// 并记下这一份物理矩形供采样使用。
fn place_bar(s: &mut BarHud) {
    let (rect, scale) = bar_rect(s);
    if let Some(handle) = HANDLE.get() {
        handle.set_placement(Some(super::rune::placement_rect(rect, scale)));
    }
    s.placed = Some((rect, scale));
}

/// 抓取条体矩形外扩折射带宽的桌面像素,过液体玻璃光学后作为背板;
/// 位置或原始像素变化时重算,静止时逐位去重。
fn capture_backdrop(s: &mut BarHud, rect: (i32, i32, i32, i32), dpi: f32) -> bool {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return s.drop_backdrop();
    }
    let pad = liquid::capture_pad(s.glass_band, s.glass.pull, dpi);
    let Some((raw_w, raw_h, raw)) =
        material::capture_rect(x - pad, y - pad, (w + 2 * pad) as u32, (h + 2 * pad) as u32)
    else {
        // 采样失败:旧桌面截图不能继续冒充当前玻璃。
        return s.drop_backdrop();
    };
    if s.backdrop_raw
        .as_ref()
        .is_some_and(|(size, previous)| *size == (raw_w, raw_h) && **previous == raw)
    {
        return false;
    }
    let t = toolbar::clamp_scale(s.bar.scale);
    let radius = toolbar::RADIUS * t * dpi;
    let mut optics = liquid::process_with_pull(
        &raw,
        raw_w,
        raw_h,
        pad,
        w as u32,
        h as u32,
        s.glass_band * dpi,
        radius,
        s.glass.pull,
    );
    liquid::prepare_glass_backdrop(&mut optics);
    let rim = liquid::environment_rim(&raw, raw_w, raw_h, pad, w as u32, h as u32, radius, dpi);
    let size = (w as u32, h as u32);
    let changed = material::set_stream(&mut s.backdrop, size, optics);
    let rim_changed = material::set_stream(&mut s.rim, size, rim);
    s.backdrop_raw = Some(((raw_w, raw_h), raw));
    let grain_changed = s.grain_size != Some(size);
    if grain_changed {
        let grain = Pixels::new();
        grain.set(size.0, size.1, liquid::grain_rgba(size.0, size.1));
        s.grain = Some(grain);
        s.grain_size = Some(size);
    }
    changed || rim_changed || grain_changed
}

/// 一帧控件树:圆角条 + 共同材质 + 一排按钮。
fn toolbar_view(s: &BarHud, cx: &mut Cx<BarHud>) -> El {
    // 减少动态效果与用户关闭特效都走即时档;reduce 来自 poll 的
    // 运行时读取,不用 host 建窗时的旧 SPI 快照。
    cx.reduced_motion(s.reduce || !s.effects);
    if !s.bar.visible {
        return el();
    }
    let t = toolbar::clamp_scale(s.bar.scale);
    let (w, h) = s.bar.size();
    let high_contrast = matches!(s.appearance, theme::Appearance::HighContrast);
    // 非高对比的玻璃与黑瓷都走候选窗同一套暗色材质墨;高对比让位系统色。
    let look = if high_contrast {
        theme::Appearance::HighContrast
    } else {
        theme::Appearance::Dark
    };
    let base = theme::palette(theme::colors_with(look, s.accent));
    let palette = match s.material {
        UiMaterial::Ceramic if !high_contrast => theme::ceramic_panel_palette(base),
        UiMaterial::Glass if !high_contrast => theme::glass_panel_palette(base),
        _ => base,
    };
    let glass = !s.private
        && s.effects
        && s.transparency
        && s.material.is_glass()
        && !high_contrast
        && s.backdrop.is_some();
    let radius = toolbar::RADIUS * t;
    let mut root = el().w_full().h_full();
    // 活背板只铺条体矩形:圆角外透明区不画,直接透出真桌面。
    if glass && let Some(backdrop) = &s.backdrop {
        root = root.child(
            el().absolute(0.0, 0.0)
                .w(w)
                .h(h)
                .continuous_corners()
                .clip_rounded(radius)
                .pixels_fit(backdrop, SurfaceFit::Fill)
                .surface_unscaled(true)
                .surface_anchor(Align::Start, Align::Start),
        );
    }
    let mut bar = el()
        .absolute(0.0, 0.0)
        .w(w)
        .h(h)
        .radius(radius)
        .continuous_corners()
        .clip_rounded(radius)
        // 条体是命中区:按下非按钮处开始拖动。
        .hit()
        .on_drag(|s: &mut BarHud, drag, _| on_bar_drag(s, drag));
    match (s.material, glass) {
        // 玻璃:与候选窗同一条像素配方(背板折射+模糊+tint+压光+边框)。
        (UiMaterial::Glass, true) => {
            bar = material::glass_face(bar, s.glass, s.frost, t);
            bar = material::glass_meniscus(bar, s.glass, t);
            if let Some(grain) = &s.grain {
                bar = material::glass_grain(bar, grain, false);
            }
            // 环境棱缘作为条体自身的图像填充:条体已显式给出宽高,
            // 纹理铺满整块面板并随条体缩放,垫在按钮与月光之下。
            if let Some(rim) = &s.rim {
                bar = bar.pixels(rim);
            }
        }
        // 黑瓷:不透的共享石墨胎体;釉光只跟真实窄带采样。
        (UiMaterial::Ceramic, _) if !high_contrast => {
            bar = material::ceramic_face(bar, &palette, s.glaze.as_ref(), t, h);
        }
        // 关闭特效、私密与高对比:实底主题兜底,不采桌面。
        _ => {
            bar = bar
                .fill(theme::srgb(palette.bg))
                .border(1.0, theme::srgb(palette.border));
        }
    }
    let size = toolbar::BUTTON * t;
    let gap = toolbar::GAP * t;
    // 缺口、亮区与暖光共用同一份月相几何:算一次,两处按各自字段取用。
    let moon = toolbar::moon_illumination(size * 0.62 / 2.0, s.phase);
    // 月亮是明确的暖黄光源:它的接光只落在按钮周围,裁在条体内,
    // 画在条体材料之后、按钮文字之前。高对比度不引入暖光。
    if !high_contrast {
        bar = bar.child(moon_light(moon, size, gap, h));
    }
    for (index, button) in toolbar::BUTTONS.into_iter().enumerate() {
        bar = bar.child(button_el(
            s,
            moon,
            button,
            index,
            size,
            gap,
            t,
            high_contrast,
            palette,
        ));
    }
    root.child(bar)
}

/// 一个按钮:居中状态字,悬停亮底,按下执行动作。全半角按钮是月亮。
/// `palette` 由 `toolbar_view` 算一次传下来,每个按钮不重复取色;
/// 位置由 `index`/`gap`/`size` 在函数内推导,不再作参数传入。
fn button_el(
    s: &BarHud,
    moon: toolbar::MoonIllumination,
    button: Button,
    index: usize,
    size: f32,
    gap: f32,
    t: f32,
    high_contrast: bool,
    palette: Palette,
) -> El {
    let x = gap + index as f32 * (size + gap);
    let y = gap;
    // 悬停亮底:普通按钮交给 El::hover 的悬停叠加(零视图提交);
    // 月亮也读 s.hover 画 fill,让挖空处透出同一块悬停底。
    let hovered = s.hover == Some(button);
    let mut cell = el()
        .absolute(x, y)
        .w(size)
        .h(size)
        .radius(size * 0.5)
        .center()
        .cursor(Cursor::Pointer)
        .role("button")
        .label(a11y_label(button))
        .key(format!("toolbar-{index}"))
        .press_on_down()
        .on_press(move |s: &mut BarHud| s.act(button));
    // 状态按钮是开关:checked 让读屏报 Toggled,value 读当前档位。
    // 设置与收起是命令按钮,不加开关语义。
    if let Some((on, value)) = a11y_toggle(button, &s.status) {
        cell = cell.checked(on).a11y_value(value);
    }
    cell = if button == Button::FullWidth {
        cell.on_hover_change(move |s: &mut BarHud, inside, _| {
            s.hover = inside.then_some(button);
        })
        .child(moon_el(moon, size, high_contrast, palette))
    } else {
        cell.hover(theme::srgb_alpha(palette.hover, 0.66)).child(
            text(toolbar::label(button, s.status))
                .font(TextStyle::new(toolbar::FONT * t, 400.0))
                .color(theme::srgb(palette.text)),
        )
    };
    if hovered {
        cell = cell.fill(theme::srgb_alpha(palette.hover, 0.66));
    }
    cell
}

/// 月亮按钮:固定的暖黄光源,不跟主题色走——候选窗的胶囊灯管才跟
/// 主题色,月亮是自己的天体。盘与月坑放在同一块 d×d 局部层里,
/// 月相只在外面用 `cutout_circle` 挖掉影圆覆盖的部分:缺月处是真
/// 透明,露出条体材料/悬停底,绝不用背景色填出一块假月盘。
/// 盘的高光是盘自身的偏心渐变;条体上的暖光由 `moon_light` 单独铺。
fn moon_el(
    moon: toolbar::MoonIllumination,
    box_size: f32,
    high_contrast: bool,
    palette: Palette,
) -> El {
    let d = box_size * 0.62;
    let r = d / 2.0;
    let at = (box_size - d) / 2.0;
    let yellow = theme::srgb(0xfac738);
    let mut disc = el().absolute(0.0, 0.0).w(d).h(d).radius(r).key("moon-disc");
    disc = if high_contrast {
        // 高对比只保留系统色月相,不加暖光。
        disc.fill(theme::srgb(palette.text))
    } else {
        disc.fill(yellow).radial_gradient(
            [0.64, 0.68],
            d,
            [
                (0.0, theme::srgb(0xffe9a0)),
                (0.38, yellow),
                (1.0, theme::srgb(0xc1871e)),
            ],
        )
    };
    let mut layer = el().absolute(at, at).w(d).h(d).child(disc);
    // 月坑:右下亮区三粒,弯月时亮区也能看见两粒。
    for (index, (fx, fy, fd)) in [(0.66, 0.70, 0.17), (0.76, 0.45, 0.12), (0.56, 0.80, 0.09)]
        .into_iter()
        .enumerate()
    {
        layer = layer.child(
            el().absolute((fx - fd / 2.0) * d, (fy - fd / 2.0) * d)
                .w(fd * d)
                .h(fd * d)
                .radius(fd * d / 2.0)
                .fill(if high_contrast {
                    theme::srgb_alpha(palette.bg, 0.55)
                } else {
                    theme::srgb_alpha(0x9a6b12, 0.32)
                })
                .key(format!("crater-{index}")),
        );
    }
    // 影圆与亮区共用同一份几何:影圆中心沿对角退到 -dist,满月时
    // 正好与盘相切;缺月处是真透明,不是背景色补丁。
    let shadow = moon.shadow_distance * toolbar::FRAC_1_SQRT_2;
    layer.key("moon").cutout_circle(
        [r - shadow, r - shadow],
        moon.shadow_radius,
        moon.shadow_radius,
    )
}

/// 条体自己的月亮接光:中心落在真实亮区质心,能量按亮区面积,
/// 范围局部并由条体圆角裁切;不是整个工具栏的天光。
fn moon_light(moon: toolbar::MoonIllumination, size: f32, gap: f32, h: f32) -> El {
    let d = size * 0.62;
    let offset = moon.centroid_offset * toolbar::FRAC_1_SQRT_2;
    let index = toolbar::BUTTONS
        .iter()
        .position(|button| *button == Button::FullWidth)
        .unwrap_or(1) as f32;
    let cx = gap + index * (size + gap) + size * 0.5 + offset;
    let cy = h * 0.5 + offset;
    let range = d * 1.6;
    let glow = d * 3.2;
    let alpha = 0.28 * moon.emitting_area_ratio;
    el().absolute(cx - glow * 0.5, cy - glow * 0.5)
        .w(glow)
        .h(glow)
        .radial_gradient(
            [0.5, 0.5],
            range,
            [
                (0.0, theme::srgb_alpha(0xfac738, alpha)),
                (0.5, theme::srgb_alpha(0xfac738, alpha * 0.5)),
                (1.0, theme::srgb_alpha(0xfac738, 0.0)),
            ],
        )
        .key("moon-light")
        .a11y_hidden()
}

/// 拖动:按下非按钮处抓住,移动跟手并夹在屏幕内,松开写回配置。
/// 屏幕坐标用 `Drag::screen_point`(桌面物理像素),与模型、配置一致。
/// 每次位置变化先作废旧位置的采样,下一轮 poll 抓新位置。
fn on_bar_drag(s: &mut BarHud, drag: &Drag) {
    let Some((sx, sy)) = drag.screen_point() else {
        return;
    };
    match drag.phase {
        Phase::Down => {
            s.bar.press(sx as f32, sy as f32);
            s.drag_moved = false;
        }
        Phase::Move => {
            let before = (s.bar.x, s.bar.y);
            let work = place::work_area_at(sx, sy);
            s.bar.drag(sx as f32, sy as f32, to_rect(work));
            if (s.bar.x, s.bar.y) != before {
                s.drag_moved = true;
                s.clear_capture();
                place_bar(s);
            }
        }
        Phase::Up => {
            s.bar.release();
            if s.drag_moved {
                save_position(&s.state, s.bar.x as i32, s.bar.y as i32);
            }
            s.drag_moved = false;
        }
    }
}

/// 上次保存的大小与位置;没有就用默认的右下角。位置夹在所在屏幕内。
fn saved_bar(state: &State) -> Toolbar {
    let (scale, pos) = {
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        (cfg.toolbar_scale, cfg.toolbar_pos)
    };
    let screen = to_rect(place::primary_work_area());
    let mut bar = Toolbar::at_default(screen).with_scale(scale, screen);
    if let Some((x, y)) = pos {
        bar.x = x as f32;
        bar.y = y as f32;
        bar = bar.clamped(to_rect(place::work_area_at(x, y)));
    }
    bar
}

/// 拖动结束后把位置写回配置并保存,下次启动回到同一个地方。
fn save_position(state: &Arc<State>, x: i32, y: i32) {
    let (before, cfg) = {
        let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
        let before = cfg.clone();
        cfg.toolbar_pos = Some((x, y));
        (before, cfg.clone())
    };
    if let Some(path) = &state.config_path
        && let Err(e) = config::save(path, &before, &cfg)
    {
        println!("保存工具栏位置 {} 失败: {e}", path.display());
    }
}

/// 按钮上的字随的状态:来自服务端的全局状态与同一份配置快照。
fn status_of(state: &State, cfg: &ConfigSnapshot) -> Status {
    Status {
        chinese: state.chinese.load(Ordering::Relaxed),
        fullwidth: state.fullwidth.load(Ordering::Relaxed),
        punct: cfg.punct,
        traditional: cfg.traditional,
    }
}

/// 简繁切换:写进配置并保存,然后应用。和标点切换走同一条路。
fn toggle_traditional(state: &Arc<State>) {
    let (before, cfg) = {
        let mut cfg = state.config.write().unwrap_or_else(|p| p.into_inner());
        let before = cfg.clone();
        cfg.traditional = !cfg.traditional;
        (before, cfg.clone())
    };
    match &state.config_path {
        Some(path) => {
            if let Err(e) = config::save(path, &before, &cfg) {
                println!("保存配置 {} 失败: {e}", path.display());
            }
        }
        None => println!("切换简繁未保存: 没有配置路径"),
    }
    crate::imp::apply_config(state, cfg);
}

/// 打开设置窗口:与语言栏的「设置」一样启动同目录的 `lueying-settings.exe`。
/// Windows 起进程要几十毫秒,不占用 UI 帧,挪到助手线程。
fn open_settings() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("open-settings".into())
        .spawn(move || {
            if let Err(e) = std::process::Command::new(dir.join("lueying-settings.exe")).spawn() {
                println!("启动设置失败: {e}");
            }
        });
}

/// 四个状态按钮的开关值与读屏值;设置、收起没有开关状态。
fn a11y_toggle(button: Button, status: &Status) -> Option<(bool, &'static str)> {
    match button {
        Button::Mode => Some((status.chinese, if status.chinese { "中文" } else { "英文" })),
        Button::FullWidth => Some((
            status.fullwidth,
            if status.fullwidth { "全角" } else { "半角" },
        )),
        Button::Punct => Some((
            status.punct,
            if status.punct {
                "中文标点"
            } else {
                "英文标点"
            },
        )),
        Button::Script => Some((
            status.traditional,
            if status.traditional {
                "繁体"
            } else {
                "简体"
            },
        )),
        Button::Settings | Button::Hide => None,
    }
}

/// 无障碍名:按钮只有状态字,读屏需要完整的动作说明。
fn a11y_label(button: Button) -> &'static str {
    match button {
        Button::Mode => "中英切换",
        Button::FullWidth => "全半角切换",
        Button::Punct => "中英文标点",
        Button::Script => "简繁切换",
        Button::Settings => "打开设置",
        Button::Hide => "收起工具栏",
    }
}

fn to_rect(r: RECT) -> Rect {
    Rect {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}
