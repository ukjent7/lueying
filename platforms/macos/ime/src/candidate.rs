//! 候选窗:无边框 NSPanel + 原生材质背板 + 自绘 NSView。
//!
//! 面板布局与绘制完全走 `lueying-render` 的平台无关实现,这里只负责
//! AppKit 窗口、坐标系翻转与鼠标命中。背板优先用 macOS 26 的液态玻璃,
//! 旧系统退回系统材质;背板由宿主提供,自绘层只画内容。候选窗不抢焦点,
//! 点击直接落在自绘视图里。

use std::cell::{Cell, RefCell};

use lueying_protocol::SessionView;
use lueying_render::panel::{
    self, Frame, Hit, Item, Layout, Metrics, Palette, PanelSpec, PreeditBar, Surface,
};
use lueying_render::spring::Spring;
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSGlassEffectView, NSGlassEffectViewStyle, NSPanel,
    NSPopUpMenuWindowLevel, NSScreen, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_foundation::{NSInteger, NSPoint, NSRect, NSSize, NSTimer};

use crate::canvas::{AppKitCanvas, AppKitMeasure};
use crate::theme;

/// 候选文字的最大绘制宽度;超长候选按这里截断。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 候选窗与插入点之间的间距。
const CARET_GAP: f64 = 4.0;
/// 出现动画的起始落差(逻辑像素);弹簧把它带回原位。
const ENTER_DISTANCE: f64 = 6.0;
/// 动画帧间隔(秒)。
const FRAME_INTERVAL: f64 = 1.0 / 60.0;
/// 背板圆角(逻辑像素)。
const CORNER_RADIUS: f64 = 10.0;
/// 出现动画的弹簧参数:刚度、阻尼、质量。
const SPRING_STIFFNESS: f32 = 360.0;
const SPRING_DAMPING: f32 = 28.0;
const SPRING_MASS: f32 = 1.0;

struct PanelModel {
    spec: PanelSpec,
    palette: Palette,
    metrics: Metrics,
    surface: Surface,
    hover: Option<usize>,
    pager: Option<Hit>,
}

#[derive(Default)]
struct CandidateViewIvars {
    model: RefCell<Option<PanelModel>>,
    target: RefCell<Option<Weak<AnyObject>>>,
}

define_class!(
    // SAFETY:NSView 的子类没有额外约束,视图只在主线程使用。
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = CandidateViewIvars]
    struct CandidateView;

    impl CandidateView {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(CandidateViewIvars::default());
            // SAFETY:初始化链只转发给 NSView。
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        /// 翻转坐标系,让绘制原点和 `lueying-render` 一样在左上角。
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let model = self.ivars().model.borrow();
            let Some(model) = model.as_ref() else {
                return;
            };
            let mut canvas = AppKitCanvas;
            let frame = Frame {
                spec: &model.spec,
                palette: &model.palette,
                metrics: model.metrics,
                surface: model.surface,
                hover: model.hover,
                pager: model.pager,
            };
            panel::draw(&mut canvas, &frame, &AppKitMeasure);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let hit = {
                let model = self.ivars().model.borrow();
                model.as_ref().map_or(Hit::Nothing, |model| {
                    panel::hit_test(
                        &model.spec,
                        &model.metrics,
                        &AppKitMeasure,
                        point.x as f32,
                        point.y as f32,
                    )
                })
            };
            let (index, kind): (NSInteger, NSInteger) = match hit {
                Hit::Item(index) => (index as NSInteger, 0),
                Hit::PrevPage => (-1, 1),
                Hit::NextPage => (-1, 2),
                Hit::Nothing => return,
            };
            let target = self
                .ivars()
                .target
                .borrow()
                .as_ref()
                .and_then(|weak| weak.load());
            if let Some(target) = target {
                // SAFETY:目标对象是控制器,方法签名与选择器一致。
                let _: () = unsafe {
                    msg_send![&*target, candidateClicked: index, kind: kind]
                };
            }
        }
    }
);

impl CandidateView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(CandidateViewIvars::default());
        // SAFETY:ivars 已初始化,这里直接调用 NSView 的初始化器。
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn set_target(&self, target: &AnyObject) {
        *self.ivars().target.borrow_mut() = Some(Weak::new(target));
    }

    fn set_model(&self, model: PanelModel) {
        *self.ivars().model.borrow_mut() = Some(model);
        self.setNeedsDisplay(true);
    }
}

/// 候选窗的背板:原生材质,候选内容画在它上面。
enum Backdrop {
    /// macOS 26 的液态玻璃;候选视图嵌在玻璃里。
    Glass(Retained<NSGlassEffectView>),
    /// 旧系统的材质视图;候选视图是它的子视图。
    Material(Retained<NSVisualEffectView>),
}

impl Backdrop {
    /// 把背板装到窗口上,并把候选视图放进去。
    fn install(&self, panel: &NSPanel, view: &CandidateView) {
        match self {
            Backdrop::Glass(glass) => {
                glass.setContentView(Some(view));
                panel.setContentView(Some(&**glass));
            }
            Backdrop::Material(material) => {
                material.addSubview(view);
                panel.setContentView(Some(&**material));
            }
        }
    }
}

/// 候选窗对象;内容视图与窗口都由它持有。
pub struct CandidatePanel {
    panel: Retained<NSPanel>,
    view: Retained<CandidateView>,
    /// 底色由谁提供。
    surface: Surface,
    /// 出现动画的纵向偏移;目标恒为 0。
    offset: RefCell<Spring>,
    /// 动画计时器。
    timer: RefCell<Option<Retained<NSTimer>>>,
    /// 候选窗是否已经显示。
    shown: Cell<bool>,
    /// 出现动画是否在跑。
    animating: Cell<bool>,
    /// 候选窗静止时的位置(屏幕坐标)。
    rest: Cell<NSPoint>,
}

impl CandidatePanel {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject) -> Self {
        let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 100.0));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            content,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY:窗口由 CandidatePanel 持有,关闭时不交给系统释放。
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setLevel(NSPopUpMenuWindowLevel);
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setWorksWhenModal(true);
        panel.setHidesOnDeactivate(true);
        panel.setIgnoresMouseEvents(false);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::Transient | NSWindowCollectionBehavior::IgnoresCycle,
        );

        let view = CandidateView::new(mtm, content);
        view.set_target(target);
        // 系统要求降低透明度或提高对比度时不透明,底色改由自绘层出。
        let surface = if reduce_transparency() || increase_contrast() {
            Surface::Solid
        } else {
            Surface::Glass
        };
        if surface == Surface::Glass {
            // 背板装上后由窗口的内容视图持有,这里不必再留一份。
            make_backdrop(mtm, content.size).install(&panel, &view);
        } else {
            panel.setContentView(Some(&view));
        }
        // 投影由材质背板或自绘层出,窗口层不再叠一层,免得出现方角影子。
        panel.setHasShadow(false);

        Self {
            panel,
            view,
            surface,
            offset: RefCell::new(Spring::new(SPRING_STIFFNESS, SPRING_DAMPING, SPRING_MASS)),
            timer: RefCell::new(None),
            shown: Cell::new(false),
            animating: Cell::new(false),
            rest: Cell::new(NSPoint::new(0.0, 0.0)),
        }
    }

    /// 按当前会话快照刷新并显示;`caret` 是屏幕坐标里的插入点行矩形。
    pub fn show(&self, view: &SessionView, caret: Option<NSRect>) {
        let metrics = Metrics::standard();
        let mut spec = spec_of(view);
        spec.width = measure_width(view, &metrics);
        spec.max_text_width = MAX_TEXT_WIDTH;
        let width = panel::panel_width(&spec, &metrics, &AppKitMeasure);
        let height = panel::panel_height(&spec, &metrics, &AppKitMeasure);
        let size = NSSize::new(f64::from(width), f64::from(height));
        self.panel.setContentSize(size);
        self.view
            .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
        self.view.set_model(PanelModel {
            spec,
            palette: theme::current(),
            metrics,
            surface: self.surface,
            hover: None,
            pager: None,
        });
        let rest = place(caret, f64::from(width), f64::from(height));
        self.rest.set(rest);
        if self.shown.get() {
            // 已经在显示:面板可能在失去焦点时被系统藏起来,重新提到前面;
            // 动画在跑就交给动画定位,否则直接落到新位置。
            self.panel.orderFrontRegardless();
            if !self.animating.get() {
                self.panel.setFrameOrigin(rest);
            }
            return;
        }
        self.shown.set(true);
        // 先摆好位置再提到前面,免得窗口在旧位置上闪一下。
        self.panel
            .setFrameOrigin(NSPoint::new(rest.x, rest.y - ENTER_DISTANCE));
        self.panel.orderFrontRegardless();
        self.start_animation();
    }

    pub fn hide(&self) {
        self.stop_timer();
        self.animating.set(false);
        self.shown.set(false);
        self.panel.orderOut(None);
    }

    /// 动画推进一帧;落定后停在原位。
    pub fn tick(&self) {
        let moving = {
            let mut offset = self.offset.borrow_mut();
            offset.step(FRAME_INTERVAL as f32)
        };
        if !moving {
            self.settle();
            return;
        }
        let offset = self.offset.borrow().value() as f64;
        let rest = self.rest.get();
        self.panel
            .setFrameOrigin(NSPoint::new(rest.x, rest.y + offset));
    }

    /// 动画收尾:停在原位。
    fn settle(&self) {
        self.animating.set(false);
        self.stop_timer();
        self.panel.setFrameOrigin(self.rest.get());
    }

    /// 开始出现动画;系统要求减少动效时直接就位。
    fn start_animation(&self) {
        self.stop_timer();
        self.offset.borrow_mut().set(-(ENTER_DISTANCE as f32));
        self.animating.set(true);
        if reduce_motion() || !self.start_timer() {
            self.settle();
        }
    }

    /// 启动逐帧回调;计时器由控制器实现 `panelTick:`。
    fn start_timer(&self) -> bool {
        let Some(target) = self.target() else {
            return false;
        };
        // SAFETY:NSTimer 保留 target;选择器在控制器上已实现。
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                FRAME_INTERVAL,
                &target,
                sel!(panelTick:),
                None,
                true,
            )
        };
        *self.timer.borrow_mut() = Some(timer);
        true
    }

    fn stop_timer(&self) {
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.invalidate();
        }
    }

    /// 控制器;候选窗靠它接收点击与动画回调。
    fn target(&self) -> Option<Retained<AnyObject>> {
        self.view
            .ivars()
            .target
            .borrow()
            .as_ref()
            .and_then(|weak| weak.load())
    }
}

/// 造一块背板:macOS 26 用液态玻璃,旧系统用系统材质。
fn make_backdrop(mtm: MainThreadMarker, size: NSSize) -> Backdrop {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
    if glass_available() {
        let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
        glass.setCornerRadius(CORNER_RADIUS);
        glass.setStyle(NSGlassEffectViewStyle::Regular);
        Backdrop::Glass(glass)
    } else {
        let material = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
        material.setMaterial(NSVisualEffectMaterial::Popover);
        material.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        material.setState(NSVisualEffectState::Active);
        // 材质视图本身是方的,圆角靠图层裁出来。
        material.setWantsLayer(true);
        if let Some(layer) = material.layer() {
            layer.setCornerRadius(CORNER_RADIUS);
            layer.setMasksToBounds(true);
        }
        Backdrop::Material(material)
    }
}

/// macOS 26 才引入液态玻璃;旧系统运行时没有这个类。
fn glass_available() -> bool {
    AnyClass::get(c"NSGlassEffectView").is_some()
}

/// 系统是否要求减少动效。
fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// 系统是否要求降低透明度。
fn reduce_transparency() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

/// 系统是否要求提高对比度。
fn increase_contrast() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast()
}

/// 当前页的候选与预编辑条。
fn spec_of(view: &SessionView) -> PanelSpec {
    let page_size = view.page_size.max(1);
    let start = (view.page * page_size).min(view.candidates.len());
    let end = (start + page_size).min(view.candidates.len());
    let items = (start..end)
        .map(|index| Item {
            text: view.candidates[index].clone(),
            hint: view.hints.get(index).cloned().unwrap_or_default(),
            selected: index == view.highlight,
        })
        .collect();
    let pages = view.candidates.len().div_ceil(page_size).max(1);
    PanelSpec {
        width: 0.0,
        max_text_width: 0.0,
        preedit: PreeditBar {
            text: view.raw.clone(),
            page_label: if pages > 1 {
                format!("{}/{}", view.page + 1, pages)
            } else {
                String::new()
            },
        },
        items,
        layout: Layout::Wrap,
    }
}

/// 全部候选参与估宽,翻页时窗口宽度保持稳定。
fn measure_width(view: &SessionView, metrics: &Metrics) -> f32 {
    let mut spec = spec_of(view);
    spec.items = view
        .candidates
        .iter()
        .enumerate()
        .map(|(index, text)| Item {
            text: text.clone(),
            hint: view.hints.get(index).cloned().unwrap_or_default(),
            selected: false,
        })
        .collect();
    panel::panel_width(&spec, metrics, &AppKitMeasure)
}

/// 候选窗摆位:默认贴在插入点下方,空间不足时翻到上方。
fn place(caret: Option<NSRect>, width: f64, height: f64) -> NSPoint {
    let Some(mtm) = MainThreadMarker::new() else {
        return NSPoint::new(0.0, 0.0);
    };
    let anchor =
        caret.unwrap_or_else(|| NSRect::new(NSEvent::mouseLocation(), NSSize::new(0.0, 18.0)));
    let mut x = anchor.origin.x;
    let mut y = anchor.origin.y - height - CARET_GAP;
    if let Some(screen) = screen_of(mtm, anchor) {
        let visible = screen.visibleFrame();
        if y < visible.origin.y {
            y = anchor.origin.y + anchor.size.height + CARET_GAP;
        }
        let max_y = visible.origin.y + visible.size.height - height;
        if y > max_y {
            y = max_y;
        }
        let max_x = visible.origin.x + visible.size.width - width;
        x = x.clamp(visible.origin.x, max_x.max(visible.origin.x));
    }
    NSPoint::new(x, y)
}

fn screen_of(mtm: MainThreadMarker, rect: NSRect) -> Option<Retained<NSScreen>> {
    let point = NSPoint::new(
        rect.origin.x + rect.size.width * 0.5,
        rect.origin.y + rect.size.height * 0.5,
    );
    let screens = NSScreen::screens(mtm);
    for index in 0..screens.count() {
        let screen = screens.objectAtIndex(index);
        let frame = screen.frame();
        if point.x >= frame.origin.x
            && point.x <= frame.origin.x + frame.size.width
            && point.y >= frame.origin.y
            && point.y <= frame.origin.y + frame.size.height
        {
            return Some(screen);
        }
    }
    NSScreen::mainScreen(mtm)
}
