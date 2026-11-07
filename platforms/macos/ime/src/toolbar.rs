//! 工具栏的 macOS 版:一条无边框的浮动面板,按钮与角色的绘制都在 `lueying-render` 里。
//!
//! 工具栏属于输入法进程:控制器激活时出现,停用时收起。按钮的动作交回发出激活的控制器,
//! 因为引擎与会话都在那里。按住拖动交给系统,松开后才判断是不是点击。
//! 动画包放在配置目录的 `toolbar-pack` 里,没有就不画角色,按钮照常。

use std::cell::RefCell;
use std::time::Instant;

use lueying_render::panel::Rect;
use lueying_render::skin_pack::{self, SkinPack};
use lueying_render::toolbar::{BUTTONS, Button, Players, Scene, Status, Toolbar, paint};
use objc2::rc::{Allocated, Retained, Weak};
use objc2::runtime::AnyObject;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSImage, NSPanel, NSPopUpMenuWindowLevel, NSScreen,
    NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSInteger, NSPoint, NSRect, NSSize, NSString, NSTimer};

use crate::canvas::AppKitCanvas;
use crate::config;
use crate::theme;

/// 逐帧回调的间隔(秒)。
const FRAME_INTERVAL: f64 = 1.0 / 30.0;

/// 工具栏的全部状态。只在主线程访问。
struct Model {
    /// 按钮与位置;坐标是翻转后的屏幕坐标(原点在左上)。
    bar: Toolbar,
    /// 工具栏能摆放的范围,同样是翻转后的屏幕坐标。
    screen: Rect,
    /// 主屏幕的高度:AppKit 的纵坐标从下往上,换算要用它。
    height: f32,
    status: Status,
    skin: Option<SkinPack>,
    sprites: Vec<Retained<NSImage>>,
    players: Players,
    now_ms: u64,
    started: Instant,
}

impl Model {
    fn new(screen: Rect, height: f32, status: Status) -> Self {
        let (skin, sprites) = load_pack();
        let bar = Toolbar::at_default(screen);
        let players = Players::new(skin.as_ref(), &bar, screen);
        Self {
            bar,
            screen,
            height,
            status,
            skin,
            sprites,
            players,
            now_ms: 0,
            started: Instant::now(),
        }
    }

    /// 角色舞台所在的矩形(翻转坐标)。
    fn stage(&self) -> Rect {
        self.bar.stage(self.screen)
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// 按时间推进两层角色;舞台上有角色时返回 true。
    fn advance(&mut self) -> bool {
        self.now_ms = self.now();
        self.players.advance(&self.bar, self.screen, self.now_ms)
    }

    /// 窗口的 AppKit 矩形:原点是左下角。
    fn window_frame(&self) -> NSRect {
        let stage = self.stage();
        NSRect::new(
            NSPoint::new(f64::from(stage.left), f64::from(self.height - stage.bottom)),
            NSSize::new(
                f64::from(stage.right - stage.left),
                f64::from(stage.bottom - stage.top),
            ),
        )
    }

    /// AppKit 的窗口矩形换回翻转坐标。
    fn flipped(&self, frame: NSRect) -> Rect {
        let top = f64::from(self.height) - frame.origin.y - frame.size.height;
        Rect::from_xywh(
            frame.origin.x as f32,
            top as f32,
            frame.size.width as f32,
            frame.size.height as f32,
        )
    }
}

/// 读动画包:`pack.json` 与帧图都在 `toolbar-pack` 里。缺了或坏了就不画角色。
fn load_pack() -> (Option<SkinPack>, Vec<Retained<NSImage>>) {
    let Some(dir) = config::toolbar_pack_dir() else {
        return (None, Vec::new());
    };
    let Ok(text) = std::fs::read_to_string(dir.join("pack.json")) else {
        return (None, Vec::new());
    };
    let skin = match skin_pack::parse(&text) {
        Ok(skin) => skin,
        Err(e) => {
            eprintln!("工具栏动画包错误: {e}");
            return (None, Vec::new());
        }
    };
    let Some(last) = skin.pack.max_frame() else {
        return (None, Vec::new());
    };
    let frames = dir.join(&skin.frames);
    let mut sprites = Vec::with_capacity(last + 1);
    for number in 0..=last {
        let path = frames.join(format!("{number}.png"));
        let Some(path_text) = path.to_str() else {
            return (None, Vec::new());
        };
        let image =
            NSImage::initWithContentsOfFile(NSImage::alloc(), &NSString::from_str(path_text));
        match image {
            Some(image) => sprites.push(image),
            None => {
                eprintln!("工具栏动画帧 {number} 读取失败: {}", path.display());
                return (None, Vec::new());
            }
        }
    }
    (Some(skin), sprites)
}

/// 主屏幕的可用区域(去掉菜单栏与 Dock),换成翻转坐标;同时返回主屏幕的高度。
fn screen_rect(mtm: MainThreadMarker) -> Option<(Rect, f32)> {
    let screens = NSScreen::screens(mtm);
    if screens.count() == 0 {
        return None;
    }
    let height = screens.objectAtIndex(0).frame().size.height;
    let visible = NSScreen::mainScreen(mtm)?.visibleFrame();
    let rect = Rect::from_xywh(
        visible.origin.x as f32,
        (height - visible.origin.y - visible.size.height) as f32,
        visible.size.width as f32,
        visible.size.height as f32,
    );
    Some((rect, height as f32))
}

#[derive(Default)]
struct ToolbarIvars {
    model: RefCell<Option<Model>>,
    /// 按钮动作发给哪个控制器。
    target: RefCell<Option<Weak<AnyObject>>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
}

define_class!(
    // SAFETY:NSView 的子类没有额外约束,视图只在主线程使用。
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ToolbarIvars]
    struct ToolbarView;

    impl ToolbarView {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: NSRect) -> Retained<Self> {
            let this = this.set_ivars(ToolbarIvars::default());
            // SAFETY:初始化链只转发给 NSView。
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }

        /// 翻转坐标系,让绘制原点与 `lueying-render` 一样在左上角。
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
            let palette = theme::current();
            let stage = model.stage();
            let scene = Scene {
                toolbar: &model.bar,
                origin: (stage.left, stage.top),
                palette: &palette,
                status: model.status,
                pack: model.skin.as_ref().map(|skin| &skin.pack),
                now_ms: model.now_ms,
                below: model.players.below_cast(),
                above: model.players.above_cast(),
            };
            let mut canvas = AppKitCanvas {
                sprites: &model.sprites,
            };
            paint(&mut canvas, &scene);
        }

        /// 逐帧回调:有角色时推进并重画。
        #[unsafe(method(toolbarTick:))]
        fn toolbar_tick(&self, _timer: &NSTimer) {
            let animated = self
                .ivars()
                .model
                .borrow_mut()
                .as_mut()
                .is_some_and(|model| model.advance());
            if animated {
                self.setNeedsDisplay(true);
            }
        }

        /// 按下就交给系统拖动;松开后没有移动就当作点击。
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let Some(window) = self.window() else {
                return;
            };
            let before = window.frame().origin;
            // 拖动期间由 AppKit 移动窗口,这一句在松开鼠标后才返回。
            window.performWindowDragWithEvent(event);
            let after = window.frame().origin;
            if after.x != before.x || after.y != before.y {
                self.moved();
            } else {
                self.click(point.x as f32, point.y as f32);
            }
        }
    }
);

impl ToolbarView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ToolbarIvars::default());
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        // SAFETY:ivars 已初始化,这里直接调用 NSView 的初始化器。
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn set_target(&self, target: &AnyObject) {
        *self.ivars().target.borrow_mut() = Some(Weak::new(target));
    }

    fn target(&self) -> Option<Retained<AnyObject>> {
        self.ivars()
            .target
            .borrow()
            .as_ref()
            .and_then(|weak| weak.load())
    }

    fn is_target(&self, object: &AnyObject) -> bool {
        self.target()
            .is_some_and(|current| std::ptr::eq(&*current, object))
    }

    fn clear_target(&self) {
        *self.ivars().target.borrow_mut() = None;
    }

    fn start_timer(&self) {
        if self.ivars().timer.borrow().is_some() {
            return;
        }
        // SAFETY:NSTimer 保留 target;选择器在本类上已实现。
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                FRAME_INTERVAL,
                self,
                sel!(toolbarTick:),
                None,
                true,
            )
        };
        *self.ivars().timer.borrow_mut() = Some(timer);
    }

    fn stop_timer(&self) {
        let timer = self.ivars().timer.borrow_mut().take();
        if let Some(timer) = timer {
            timer.invalidate();
        }
    }

    /// 拖动结束:夹回屏幕里,把窗口摆到新位置。
    fn moved(&self) {
        let Some(window) = self.window() else {
            return;
        };
        let frame = window.frame();
        let mut guard = self.ivars().model.borrow_mut();
        let Some(model) = guard.as_mut() else {
            return;
        };
        let dragged = model.flipped(frame);
        let old = model.stage();
        model.bar.x += dragged.left - old.left;
        model.bar.y += dragged.top - old.top;
        model.bar = model.bar.clamped(model.screen);
        window.setFrameOrigin(model.window_frame().origin);
    }

    /// 点击:先看按钮,再看角色(上层优先)。
    fn click(&self, x: f32, y: f32) {
        let button = {
            let mut guard = self.ivars().model.borrow_mut();
            let Some(model) = guard.as_mut() else {
                return;
            };
            let stage = model.stage();
            let (sx, sy) = (x + stage.left, y + stage.top);
            match model.bar.hit(sx, sy) {
                Some(button) => Some(button),
                None => {
                    model.players.click(sx, sy);
                    None
                }
            }
        };
        if let Some(button) = button {
            self.send(button);
        }
    }

    /// 把按钮的动作交给控制器;编号是按钮在 `BUTTONS` 里的位置。
    fn send(&self, button: Button) {
        let Some(index) = BUTTONS.iter().position(|b| *b == button) else {
            return;
        };
        if let Some(target) = self.target() {
            // SAFETY:目标是控制器,`toolbarAction:` 的签名与此一致。
            let _: () = unsafe { msg_send![&*target, toolbarAction: index as NSInteger] };
        }
    }
}

/// 面板:无边框、不抢焦点、浮在普通窗口之上;内容就是工具栏视图。
fn make_panel(mtm: MainThreadMarker, view: &Retained<ToolbarView>) -> Retained<NSPanel> {
    let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 60.0));
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        content,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY:窗口由 bar 持有,关闭时不交给系统释放。
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setLevel(NSPopUpMenuWindowLevel);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setWorksWhenModal(true);
    panel.setHidesOnDeactivate(false);
    // 背景与投影都由自绘层出,窗口层不再叠阴影,免得出现方角影子。
    panel.setHasShadow(false);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel.setContentView(Some(view));
    panel
}

struct Bar {
    panel: Retained<NSPanel>,
    view: Retained<ToolbarView>,
}

thread_local! {
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
}

/// 已经建好的面板与视图;还没建过就返回 `None`。
fn parts() -> Option<(Retained<NSPanel>, Retained<ToolbarView>)> {
    BAR.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|bar| (bar.panel.clone(), bar.view.clone()))
    })
}

/// 显示工具栏(控制器激活时调用)。收起过的工具栏在下一次激活时重新出现;拖过的位置保留。
pub fn show(mtm: MainThreadMarker, target: &AnyObject, status: Status) {
    let Some((screen, height)) = screen_rect(mtm) else {
        return;
    };
    let (panel, view) = BAR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some(bar) = slot.as_ref() {
            return (bar.panel.clone(), bar.view.clone());
        }
        let view = ToolbarView::new(mtm);
        let panel = make_panel(mtm, &view);
        *slot = Some(Bar {
            panel: panel.clone(),
            view: view.clone(),
        });
        (panel, view)
    });
    view.set_target(target);
    let frame = {
        let mut guard = view.ivars().model.borrow_mut();
        let model = guard.get_or_insert_with(|| Model::new(screen, height, status));
        model.height = height;
        model.status = status;
        model.screen = screen;
        model.bar.visible = true;
        model.bar = model.bar.clamped(screen);
        model.window_frame()
    };
    panel.setContentSize(frame.size);
    view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
    panel.setFrameOrigin(frame.origin);
    panel.orderFrontRegardless();
    view.start_timer();
    view.setNeedsDisplay(true);
}

/// 收起工具栏:隐藏窗口、停止计时。
pub fn collapse() {
    let Some((panel, view)) = parts() else {
        return;
    };
    if let Some(model) = view.ivars().model.borrow_mut().as_mut() {
        model.bar.visible = false;
    }
    view.stop_timer();
    panel.orderOut(None);
}

/// 控制器停用时调用:只有当前发出激活的那个控制器才能收起工具栏。
pub fn hide_for(target: &AnyObject) {
    let Some((panel, view)) = parts() else {
        return;
    };
    if !view.is_target(target) {
        return;
    }
    view.clear_target();
    view.stop_timer();
    panel.orderOut(None);
}

/// 同步按钮上的字(中英、全半、标点、简繁)。
pub fn update(status: Status) {
    let Some((_, view)) = parts() else {
        return;
    };
    if let Some(model) = view.ivars().model.borrow_mut().as_mut() {
        model.status = status;
    }
    view.setNeedsDisplay(true);
}

/// 用户按了键:舞台上的角色都收到一次按键,于是停下来看着输入。
pub fn key_pressed() {
    let Some((_, view)) = parts() else {
        return;
    };
    if let Some(model) = view.ivars().model.borrow_mut().as_mut() {
        let now = model.now();
        model.players.key(now);
    }
}
