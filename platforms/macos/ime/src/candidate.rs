//! 候选窗:无边框 NSPanel + 自绘 NSView。
//!
//! 面板布局与绘制完全走 `lueying-render` 的平台无关实现,这里只负责
//! AppKit 窗口、坐标系翻转与鼠标命中。候选窗不抢焦点,点击直接落在
//! 自绘视图里。

use std::cell::RefCell;

use lueying_protocol::SessionView;
use lueying_render::panel::{self, Frame, Hit, Item, Layout, Metrics, Palette, PanelSpec, PreeditBar};
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSPanel, NSScreen, NSView, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSPopUpMenuWindowLevel,
};
use objc2_foundation::{MainThreadMarker, NSInteger, NSPoint, NSRect, NSSize};

use crate::canvas::{AppKitCanvas, AppKitMeasure};
use crate::theme;

/// 候选文字的最大绘制宽度;超长候选按这里截断。
const MAX_TEXT_WIDTH: f32 = 490.0;
/// 候选窗与插入点之间的间距。
const CARET_GAP: f64 = 4.0;

struct PanelModel {
    spec: PanelSpec,
    palette: Palette,
    metrics: Metrics,
    hover: Option<usize>,
    pager: Option<Hit>,
}

struct CandidateViewIvars {
    model: RefCell<Option<PanelModel>>,
    target: RefCell<Option<Weak<AnyObject>>>,
}

impl Default for CandidateViewIvars {
    fn default() -> Self {
        Self {
            model: RefCell::new(None),
            target: RefCell::new(None),
        }
    }
}

define_class!(
    // SAFETY:NSView 的子类没有额外约束,视图只在主线程使用。
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = CandidateViewIvars]
    struct CandidateView;

    impl CandidateView {
        #[unsafe(method(initWithFrame:))]
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
        // SAFETY:alloc/init 成对调用,initWithFrame: 会初始化 ivars。
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }

    fn set_target(&self, target: &AnyObject) {
        *self.ivars().target.borrow_mut() = Some(Weak::new(target));
    }

    fn set_model(&self, model: PanelModel) {
        *self.ivars().model.borrow_mut() = Some(model);
        self.setNeedsDisplay(true);
    }
}

/// 候选窗对象;内容视图与窗口都由它持有。
pub struct CandidatePanel {
    panel: Retained<NSPanel>,
    view: Retained<CandidateView>,
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
        panel.setHasShadow(false);
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
        panel.setContentView(Some(&view));
        Self { panel, view }
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
            hover: None,
            pager: None,
        });
        self.panel
            .setFrameOrigin(place(caret, f64::from(width), f64::from(height)));
        self.panel.orderFrontRegardless();
    }

    pub fn hide(&self) {
        self.panel.orderOut(None);
    }
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
    let anchor = caret.unwrap_or_else(|| {
        NSRect::new(NSEvent::mouseLocation(), NSSize::new(0.0, 18.0))
    });
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
