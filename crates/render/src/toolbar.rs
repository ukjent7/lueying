//! 输入法工具栏:一排常用按钮的悬浮条,默认在屏幕右下角,可以拖动。平台无关。
//!
//! 按钮与传统输入法的工具栏一致:中/英、全/半角、中英标点、简/繁、设置,另补「收起」。
//! 位置记在 [`Toolbar::pos`] 里,拖动时夹在屏幕内;默认位置在右下角、任务栏上方。
//! 外观可以由扩展自定义,但按钮的功能不能丢,所以这里只定义按钮与动作,不定义画法。

use crate::panel::Rect;

/// 按钮的边长(逻辑像素)。
pub const BUTTON: f32 = 28.0;
/// 按钮之间与边缘的留白。
pub const GAP: f32 = 4.0;
/// 默认离屏幕右边与底边的距离;底边要让开任务栏,这里取一个保守的值。
pub const MARGIN: f32 = 12.0;
/// 任务栏高度的保守估计,默认位置要在它上方。
pub const TASKBAR: f32 = 48.0;

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

/// 工具栏的状态:是否显示、左上角位置、拖动中的偏移。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Toolbar {
    pub visible: bool,
    pub x: f32,
    pub y: f32,
    /// 拖动中:指针相对左上角的偏移;没有拖动时为 `None`。
    grab: Option<(f32, f32)>,
}

impl Toolbar {
    /// 工具栏的总宽高(像素)。
    pub fn size() -> (f32, f32) {
        let width = BUTTONS.len() as f32 * BUTTON + (BUTTONS.len() as f32 + 1.0) * GAP;
        (width, BUTTON + 2.0 * GAP)
    }

    /// 放在屏幕右下角、任务栏上方的默认位置。
    pub fn at_default(screen: Rect) -> Self {
        let (width, height) = Self::size();
        Self {
            visible: true,
            x: screen.right - width - MARGIN,
            y: screen.bottom - height - TASKBAR,
            grab: None,
        }
        .clamped(screen)
    }

    /// 工具栏占的矩形。
    pub fn rect(&self) -> Rect {
        let (width, height) = Self::size();
        Rect::from_xywh(self.x, self.y, width, height)
    }

    /// 每个按钮的矩形,从左到右。
    pub fn buttons(&self) -> Vec<(Button, Rect)> {
        BUTTONS
            .iter()
            .enumerate()
            .map(|(index, &button)| {
                let x = self.x + GAP + index as f32 * (BUTTON + GAP);
                let y = self.y + GAP;
                (button, Rect::from_xywh(x, y, BUTTON, BUTTON))
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

    /// 在工具栏的任意非按钮处按下时开始拖动。按下在按钮上不拖动,由点击处理。
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

    /// 把位置夹在屏幕内(屏幕分辨率变了、或者拖出去了,都要拉回来)。
    pub fn clamped(mut self, screen: Rect) -> Self {
        let (width, height) = Self::size();
        self.x = self
            .x
            .clamp(screen.left, (screen.right - width).max(screen.left));
        self.y = self
            .y
            .clamp(screen.top, (screen.bottom - height).max(screen.top));
        self
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
    fn default_position_is_bottom_right_above_the_taskbar() {
        let bar = Toolbar::at_default(screen());
        let rect = bar.rect();
        assert!(rect.right <= 1920.0 - MARGIN + 0.001);
        assert!((rect.bottom - (1080.0 - TASKBAR)).abs() < 0.001);
        assert!(rect.left > 1920.0 / 2.0);
    }

    #[test]
    fn each_button_hits_its_own_action() {
        let bar = Toolbar::at_default(screen());
        for (button, rect) in bar.buttons() {
            let (cx, cy) = (
                (rect.left + rect.right) / 2.0,
                (rect.top + rect.bottom) / 2.0,
            );
            assert_eq!(bar.hit(cx, cy), Some(button));
            assert_eq!(
                button.action(),
                match button {
                    Button::Mode => Action::ToggleMode,
                    Button::FullWidth => Action::ToggleFullWidth,
                    Button::Punct => Action::TogglePunct,
                    Button::Script => Action::ToggleScript,
                    Button::Settings => Action::OpenSettings,
                    Button::Hide => Action::Hide,
                }
            );
        }
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
        assert!((bar.x - (100.0 - 1.0)).abs() < 0.001);
        assert!((bar.y - (200.0 - 1.0)).abs() < 0.001);
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
