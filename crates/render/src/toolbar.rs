//! 输入法工具栏:一排常用按钮的悬浮条,默认在屏幕右下角,可以拖动,大小可以在设置里调。平台无关。
//!
//! 按钮与传统输入法的工具栏一致:中/英、全/半角、中英标点、简/繁、设置,另补「收起」。
//! 大小用倍率表示,按钮与间距一起缩放,倍率只能在 [`SCALE_MIN`, `SCALE_MAX`] 之间,
//! 不会被设成无限大或无限小。

use crate::paint::{Canvas, Color};
use crate::panel::{Palette, Rect};

/// 按钮的边长(倍率 1.0 时,逻辑像素)。
pub const BUTTON: f32 = 28.0;
/// 按钮之间与边缘的留白(倍率 1.0 时)。
pub const GAP: f32 = 4.0;
/// 默认离屏幕右边与底边的距离。
pub const MARGIN: f32 = 12.0;
/// 任务栏高度的保守估计,默认位置要在它上方。
pub const TASKBAR: f32 = 48.0;
/// 工具栏大小倍率的下限与上限:用户只能在这个范围内调。
pub const SCALE_MIN: f32 = 0.75;
pub const SCALE_MAX: f32 = 1.5;

/// 把倍率夹到允许的范围内。设置页的输入、配置文件里的值都要过这一关。
pub fn clamp_scale(scale: f32) -> f32 {
    if scale.is_nan() {
        return 1.0;
    }
    scale.clamp(SCALE_MIN, SCALE_MAX)
}

/// 工具栏上的按钮。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// 中 / 英 切换。
    Mode,
    /// 全角 / 半角 切换。
    FullWidth,
    /// 中文 / 英文标点切换。
    Punct,
    /// 简 / 繁 切换。
    Script,
    /// 打开设置。
    Settings,
    /// 收起工具栏(再次显示要从设置或快捷键来)。
    Hide,
}

/// 按钮被点击后要执行的动作。宿主把它接到输入引擎与设置窗口上。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleMode,
    ToggleFullWidth,
    TogglePunct,
    ToggleScript,
    OpenSettings,
    Hide,
}

/// 按钮从左到右的顺序。
pub const BUTTONS: [Button; 6] = [
    Button::Mode,
    Button::FullWidth,
    Button::Punct,
    Button::Script,
    Button::Settings,
    Button::Hide,
];

impl Button {
    /// 按钮对应的动作。
    pub fn action(self) -> Action {
        match self {
            Button::Mode => Action::ToggleMode,
            Button::FullWidth => Action::ToggleFullWidth,
            Button::Punct => Action::TogglePunct,
            Button::Script => Action::ToggleScript,
            Button::Settings => Action::OpenSettings,
            Button::Hide => Action::Hide,
        }
    }
}

/// 工具栏的状态:是否显示、大小倍率、左上角位置、拖动中的偏移。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Toolbar {
    pub visible: bool,
    pub scale: f32,
    pub x: f32,
    pub y: f32,
    /// 拖动中:指针相对左上角的偏移;没有拖动时为 `None`。
    grab: Option<(f32, f32)>,
}

impl Toolbar {
    /// 工具栏的总宽高(像素),按当前倍率。
    pub fn size(&self) -> (f32, f32) {
        let s = clamp_scale(self.scale);
        let width = BUTTONS.len() as f32 * BUTTON * s + (BUTTONS.len() as f32 + 1.0) * GAP * s;
        (width, (BUTTON + 2.0 * GAP) * s)
    }

    /// 放在屏幕右下角、任务栏上方的默认位置(倍率 1.0)。
    pub fn at_default(screen: Rect) -> Self {
        let bar = Self {
            visible: true,
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            grab: None,
        };
        let (width, height) = bar.size();
        Self {
            x: screen.right - width - MARGIN,
            y: screen.bottom - height - TASKBAR,
            ..bar
        }
        .clamped(screen)
    }

    /// 工具栏占的矩形。
    pub fn rect(&self) -> Rect {
        let (width, height) = self.size();
        Rect::from_xywh(self.x, self.y, width, height)
    }

    /// 每个按钮的矩形,从左到右。
    pub fn buttons(&self) -> Vec<(Button, Rect)> {
        let s = clamp_scale(self.scale);
        let size = BUTTON * s;
        let gap = GAP * s;
        BUTTONS
            .iter()
            .enumerate()
            .map(|(index, &button)| {
                let x = self.x + gap + index as f32 * (size + gap);
                let y = self.y + gap;
                (button, Rect::from_xywh(x, y, size, size))
            })
            .collect()
    }

    /// 点中了哪个按钮;工具栏隐藏时什么也点不中。
    pub fn hit(&self, px: f32, py: f32) -> Option<Button> {
        if !self.visible {
            return None;
        }
        self.buttons()
            .into_iter()
            .find(|(_, rect)| inside(*rect, px, py))
            .map(|(button, _)| button)
    }

    /// 在工具栏的非按钮处按下时开始拖动。按在按钮上不拖动,由点击处理。
    pub fn press(&mut self, px: f32, py: f32) {
        if self.visible && inside(self.rect(), px, py) && self.hit(px, py).is_none() {
            self.grab = Some((px - self.x, py - self.y));
        }
    }

    /// 拖动中:指针移到 `(px, py)`,工具栏跟过去,并夹在屏幕内。
    pub fn drag(&mut self, px: f32, py: f32, screen: Rect) {
        if let Some((dx, dy)) = self.grab {
            self.x = px - dx;
            self.y = py - dy;
            *self = self.clamped(screen);
        }
    }

    /// 松开鼠标,结束拖动。
    pub fn release(&mut self) {
        self.grab = None;
    }

    /// 设置页改大小:倍率先夹到允许范围,再把位置拉回屏幕内。
    pub fn with_scale(mut self, scale: f32, screen: Rect) -> Self {
        self.scale = clamp_scale(scale);
        self.clamped(screen)
    }

    /// 把位置夹在屏幕内(屏幕分辨率变了、或者拖出去了,都要拉回来)。
    pub fn clamped(mut self, screen: Rect) -> Self {
        let (width, height) = self.size();
        self.x = self
            .x
            .clamp(screen.left, (screen.right - width).max(screen.left));
        self.y = self
            .y
            .clamp(screen.top, (screen.bottom - height).max(screen.top));
        self
    }
}

/// 按钮上的字随的状态:中英、全半角、中英标点、简繁。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub chinese: bool,
    pub fullwidth: bool,
    pub punct: bool,
    pub traditional: bool,
}

/// 按钮当前显示的字:随状态变,例如中/英、全/半。
pub fn label(button: Button, status: Status) -> &'static str {
    match button {
        Button::Mode if status.chinese => "中",
        Button::Mode => "英",
        Button::FullWidth if status.fullwidth => "全",
        Button::FullWidth => "半",
        Button::Punct if status.punct => "，",
        Button::Punct => ",",
        Button::Script if status.traditional => "繁",
        Button::Script => "简",
        Button::Settings => "设",
        Button::Hide => "×",
    }
}

/// 圆角半径与字号(倍率 1.0 时)。
const RADIUS: f32 = 9.0;
const FONT: f32 = 14.0;

/// 一帧工具栏的绘制输入。坐标都是屏幕坐标,`origin` 是窗口左上角,画的时候要减去它。
pub struct Scene<'a> {
    pub toolbar: &'a Toolbar,
    pub origin: (f32, f32),
    pub palette: &'a Palette,
    pub status: Status,
}

/// 画一帧:背景与按钮。
pub fn paint(canvas: &mut dyn Canvas, scene: &Scene) {
    let scale = scene.toolbar.scale;
    let bar = local(scene.toolbar.rect(), scene.origin);
    canvas.fill_rrect(bar, RADIUS * scale, Color::opaque(scene.palette.bg));
    canvas.stroke_rrect(
        bar,
        RADIUS * scale,
        1.0,
        Color::opaque(scene.palette.border),
    );
    for (button, rect) in scene.toolbar.buttons() {
        let text = label(button, scene.status);
        let color = Color::opaque(scene.palette.text);
        canvas.text_centered(text, local(rect, scene.origin), FONT * scale, color);
    }
}

/// 屏幕坐标换成窗口内的局部坐标。
fn local(rect: Rect, origin: (f32, f32)) -> Rect {
    Rect {
        left: rect.left - origin.0,
        top: rect.top - origin.1,
        right: rect.right - origin.0,
        bottom: rect.bottom - origin.1,
    }
}

fn inside(rect: Rect, px: f32, py: f32) -> bool {
    px >= rect.left && px < rect.right && py >= rect.top && py < rect.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rect {
        Rect::from_xywh(0.0, 0.0, 1920.0, 1080.0)
    }

    #[test]
    fn labels_follow_the_status() {
        let status = Status {
            chinese: false,
            fullwidth: true,
            punct: false,
            traditional: true,
        };
        assert_eq!(label(Button::Mode, status), "英");
        assert_eq!(label(Button::FullWidth, status), "全");
        assert_eq!(label(Button::Punct, status), ",");
        assert_eq!(label(Button::Script, status), "繁");
    }

    #[test]
    fn default_position_is_bottom_right_above_the_taskbar() {
        let bar = Toolbar::at_default(screen());
        let rect = bar.rect();
        assert!(rect.right <= 1920.0 - MARGIN + 0.001);
        assert!((rect.bottom - (1080.0 - TASKBAR)).abs() < 0.001);
        assert!(rect.left > 1920.0 / 2.0);
    }

    #[test]
    fn scale_is_clamped_so_the_bar_cannot_grow_or_shrink_without_limit() {
        assert_eq!(clamp_scale(0.1), SCALE_MIN);
        assert_eq!(clamp_scale(10.0), SCALE_MAX);
        assert_eq!(clamp_scale(f32::NAN), 1.0);
        let big = Toolbar::at_default(screen()).with_scale(99.0, screen());
        assert_eq!(big.scale, SCALE_MAX);
        let small = Toolbar::at_default(screen()).with_scale(0.0, screen());
        assert_eq!(small.scale, SCALE_MIN);
    }

    #[test]
    fn larger_scale_makes_the_bar_larger_proportionally() {
        let normal = Toolbar::at_default(screen());
        let large = normal.with_scale(1.5, screen());
        let (w1, h1) = normal.size();
        let (w2, h2) = large.size();
        assert!((w2 - w1 * 1.5).abs() < 0.001);
        assert!((h2 - h1 * 1.5).abs() < 0.001);
    }

    #[test]
    fn each_button_hits_its_own_action_at_every_scale() {
        for scale in [SCALE_MIN, 1.0, SCALE_MAX] {
            let bar = Toolbar::at_default(screen()).with_scale(scale, screen());
            for (button, rect) in bar.buttons() {
                let (cx, cy) = (
                    (rect.left + rect.right) / 2.0,
                    (rect.top + rect.bottom) / 2.0,
                );
                assert_eq!(bar.hit(cx, cy), Some(button));
            }
        }
        assert_eq!(Button::Hide.action(), Action::Hide);
        assert_eq!(Button::Mode.action(), Action::ToggleMode);
    }

    #[test]
    fn hidden_toolbar_catches_no_clicks() {
        let mut bar = Toolbar::at_default(screen());
        bar.visible = false;
        let (cx, cy) = (bar.x + GAP + BUTTON / 2.0, bar.y + GAP + BUTTON / 2.0);
        assert_eq!(bar.hit(cx, cy), None);
    }

    #[test]
    fn dragging_moves_the_bar_and_keeps_it_on_screen() {
        let mut bar = Toolbar::at_default(screen());
        let (cx, cy) = (bar.x + 1.0, bar.y + 1.0);
        bar.press(cx, cy);
        bar.drag(100.0, 200.0, screen());
        assert!((bar.x - 99.0).abs() < 0.001);
        assert!((bar.y - 199.0).abs() < 0.001);
        bar.drag(-500.0, -500.0, screen());
        assert_eq!((bar.x, bar.y), (0.0, 0.0));
        bar.release();
        bar.drag(900.0, 900.0, screen());
        assert_eq!((bar.x, bar.y), (0.0, 0.0), "松手后不再跟着走");
    }

    #[test]
    fn pressing_a_button_does_not_start_a_drag() {
        let mut bar = Toolbar::at_default(screen());
        let before = (bar.x, bar.y);
        let (cx, cy) = (bar.x + GAP + BUTTON / 2.0, bar.y + GAP + BUTTON / 2.0);
        bar.press(cx, cy);
        bar.drag(10.0, 10.0, screen());
        assert_eq!((bar.x, bar.y), before);
    }

    #[test]
    fn a_screen_change_pulls_the_bar_back_inside() {
        let bar = Toolbar {
            visible: true,
            scale: 1.5,
            x: 1800.0,
            y: 1000.0,
            grab: None,
        };
        let small = Rect::from_xywh(0.0, 0.0, 800.0, 600.0);
        let fixed = bar.clamped(small);
        assert!(fixed.rect().right <= 800.0 + 0.001);
        assert!(fixed.rect().bottom <= 600.0 + 0.001);
    }
}
