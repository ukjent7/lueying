//! 设置窗的控件层:页面描述、布局、绘制与命中。
//!
//! 设置窗的结构是「一页说明 + 每行一个控件」,不需要通用 GUI 框架:
//! [`Page`] 描述内容,[`layout`] 算几何,[`Painter`] 出帧,[`Layout::hit_page`]
//! 与 [`Layout::hit_card`] 把点击反查回控件。平台侧只管窗口、事件与状态,
//! 画面与命中都在这里,Linux E2E 因此能直接断言布局与像素。

use skia_safe::{Font, Rect};

#[path = "ui_paint.rs"]
mod paint;

pub use paint::Painter;

/// 页面宽度(逻辑像素)。
pub const PAGE_WIDTH: f32 = 760.0;
/// 页面留白。
const PAD: f32 = 18.0;
/// 头部、卡片与页脚之间的间距。
const GAP: f32 = 10.0;
/// 左栏宽度与卡片之间的间距。
const SIDEBAR_W: f32 = 168.0;
const SIDEBAR_GAP: f32 = 16.0;
/// 左栏内边距、每项高度与项间距。
const SIDEBAR_PAD: f32 = 8.0;
const NAV_H: f32 = 34.0;
const NAV_GAP: f32 = 2.0;
/// 品牌图标边长。
const LOGO: f32 = 36.0;
/// 分组标题的上下留白、字号与行高。
const SECTION_TOP: f32 = 7.0;
const SECTION_BOTTOM: f32 = 2.0;
const SECTION_SIZE: f32 = 11.0;
const SECTION_LINE: f32 = 15.0;
/// 设置行的高度与左右留白。
const ROW_H: f32 = 39.0;
const ROW_PAD: f32 = 14.0;
/// 行标题与说明的字号;说明文字的最大宽度。
const LABEL_SIZE: f32 = 13.5;
const DETAIL_SIZE: f32 = 12.0;
const DETAIL_MAX: f32 = 230.0;
/// 开关与旋钮。
const TOGGLE_W: f32 = 38.0;
const TOGGLE_H: f32 = 22.0;
const KNOB: f32 = 16.0;
/// 分段格与分段组。
const SEGMENT_H: f32 = 26.0;
const SEGMENT_PAD: f32 = 2.0;
const SEGMENT_GAP: f32 = 2.0;
/// 按钮高度与左右留白。
const BUTTON_H: f32 = 30.0;
const BUTTON_PAD: f32 = 12.0;
/// 同一行里按钮之间的间距。
const BUTTON_GAP: f32 = 6.0;
/// 圆角。
const RADIUS: f32 = 6.0;
const LOGO_RADIUS: f32 = 8.0;
const CARD_RADIUS: f32 = 10.0;

/// 控件标识:同类控件共用前缀,组内按下标区分。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Id {
    /// 前缀:一组同类控件共用。
    pub base: &'static str,
    /// 组内下标。
    pub index: usize,
}

impl Id {
    pub fn new(base: &'static str, index: usize) -> Self {
        Self { base, index }
    }
}

/// 按钮外观。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// 一个按钮。
#[derive(Clone, Debug)]
pub struct Button {
    pub id: Id,
    pub text: String,
    pub kind: ButtonKind,
    pub enabled: bool,
}

/// 分段组里的一格。
#[derive(Clone, Debug)]
pub struct SegmentOption {
    pub text: String,
    /// 选中时的文字颜色;None 用默认色。
    pub tint: Option<u32>,
}

impl SegmentOption {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tint: None,
        }
    }

    /// 带固定文字颜色的选项(主题色一列用)。
    pub fn tinted(text: impl Into<String>, tint: u32) -> Self {
        Self {
            text: text.into(),
            tint: Some(tint),
        }
    }
}

/// 行右侧的控件。
#[derive(Clone, Debug)]
pub enum Control {
    /// 开关。
    Toggle { id: Id, on: bool },
    /// 分段组;每格按下标有自己的 id,`selected` 是选中格。
    Segment {
        id: Id,
        options: Vec<SegmentOption>,
        selected: usize,
        /// 每格宽度(逻辑像素)。
        width: f32,
    },
    /// 一排按钮。
    Buttons { items: Vec<Button> },
    /// 按钮与开关并排:开关贴行右,按钮在它左边。
    ButtonsToggle {
        items: Vec<Button>,
        toggle: Id,
        toggle_on: bool,
    },
    /// 右侧没有控件。
    None,
}

/// 一行设置:左边标题与说明,右边一个控件。
#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    pub detail: String,
    pub control: Control,
}

/// 分组:小标题 + 若干行。
#[derive(Clone, Debug)]
pub struct Group {
    pub title: String,
    pub rows: Vec<Row>,
}

/// 左栏的一个分类入口。
#[derive(Clone, Debug)]
pub struct NavItem {
    pub id: Id,
    pub text: String,
}

/// 页面:头部、左栏导航、分组与页脚。
#[derive(Clone, Debug)]
pub struct Page {
    pub title: String,
    pub subtitle: String,
    /// 头部右侧的按钮(检查更新)。
    pub header_button: Option<Button>,
    /// 左栏分类导航。
    pub nav: Vec<NavItem>,
    /// 当前选中的左栏下标。
    pub active_nav: usize,
    pub groups: Vec<Group>,
    /// 页脚提示文字与颜色。
    pub notice: String,
    pub notice_tint: u32,
    /// 页脚右侧的按钮(关闭、保存)。
    pub footer_buttons: Vec<Button>,
}

impl Page {
    /// 按 id 找按钮;分段格与开关不算按钮。
    pub fn button(&self, id: Id) -> Option<&Button> {
        if let Some(button) = &self.header_button
            && button.id == id
        {
            return Some(button);
        }
        for button in &self.footer_buttons {
            if button.id == id {
                return Some(button);
            }
        }
        for group in &self.groups {
            for row in &group.rows {
                let items = match &row.control {
                    Control::Buttons { items } => items,
                    Control::ButtonsToggle { items, .. } => items,
                    _ => continue,
                };
                if let Some(button) = items.iter().find(|button| button.id == id) {
                    return Some(button);
                }
            }
        }
        None
    }

    /// 页面上有没有这个控件:开关、分段组、按钮都算。
    pub fn has_control(&self, id: Id) -> bool {
        if let Some(button) = &self.header_button
            && button.id == id
        {
            return true;
        }
        if self.footer_buttons.iter().any(|button| button.id == id) {
            return true;
        }
        if self.nav.iter().any(|item| item.id == id) {
            return true;
        }
        for group in &self.groups {
            for row in &group.rows {
                match &row.control {
                    Control::Toggle { id: control, .. } => {
                        if *control == id {
                            return true;
                        }
                    }
                    Control::Segment { id: control, .. } => {
                        // 分段组里的每一格都算这个组的控件。
                        if control.base == id.base {
                            return true;
                        }
                    }
                    Control::Buttons { items } => {
                        if items.iter().any(|button| button.id == id) {
                            return true;
                        }
                    }
                    Control::ButtonsToggle { items, toggle, .. } => {
                        if *toggle == id || items.iter().any(|button| button.id == id) {
                            return true;
                        }
                    }
                    Control::None => {}
                }
            }
        }
        false
    }

    /// Tab 次序:头部 → 左栏 → 当前页控件 → 页脚。
    pub fn focus_ids(&self) -> Vec<Id> {
        let mut order = Vec::new();
        if let Some(button) = &self.header_button
            && button.enabled
        {
            order.push(button.id);
        }
        order.extend(self.nav.iter().map(|item| item.id));
        for group in &self.groups {
            for row in &group.rows {
                match &row.control {
                    Control::Toggle { id, .. } => order.push(*id),
                    Control::Segment { id, .. } => order.push(Id::new(id.base, 0)),
                    Control::Buttons { items } => {
                        order.extend(items.iter().filter(|b| b.enabled).map(|b| b.id));
                    }
                    Control::ButtonsToggle { items, toggle, .. } => {
                        order.extend(items.iter().filter(|b| b.enabled).map(|b| b.id));
                        order.push(*toggle);
                    }
                    Control::None => {}
                }
            }
        }
        order.extend(
            self.footer_buttons
                .iter()
                .filter(|button| button.enabled)
                .map(|button| button.id),
        );
        order
    }
}

/// 布局结果:几何都在物理像素里。
#[derive(Clone, Debug, Default)]
pub struct Layout {
    /// 页面自然总高(物理像素);比视口高时可滚动。
    pub height: f32,
    /// 卡片在页面里的位置(自然位置,还没滚动)。
    pub card: Rect,
    /// 左栏导航面板的可视区域。
    pub sidebar: Rect,
    /// 滚动区:视口里露出来的那一段(页面坐标)。
    pub scroll: Rect,
    /// 页脚:视口底部的一条。
    pub footer: Rect,
    /// 品牌图标。
    pub logo: Rect,
    /// 固定部分(头部、页脚)的可点区域。
    page_hits: Vec<(Id, Rect)>,
    /// 卡片里的可点区域(卡片坐标)。
    card_hits: Vec<(Id, Rect)>,
    /// 固定部分的焦点环矩形。
    page_rings: Vec<(Id, Rect)>,
    /// 卡片里的焦点环矩形(卡片坐标)。
    card_rings: Vec<(Id, Rect)>,
}

impl Layout {
    /// 固定部分命中;页面坐标。
    pub fn hit_page(&self, x: f32, y: f32) -> Option<Id> {
        pick(&self.page_hits, x, y)
    }

    /// 卡片命中;卡片坐标。
    pub fn hit_card(&self, x: f32, y: f32) -> Option<Id> {
        pick(&self.card_hits, x, y)
    }

    /// 固定部分的焦点环矩形。
    pub fn ring_page(&self, id: Id) -> Option<Rect> {
        self.page_rings
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, rect)| *rect)
    }

    /// 卡片里的焦点环矩形。
    pub fn ring_card(&self, id: Id) -> Option<Rect> {
        self.card_rings
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, rect)| *rect)
    }

    /// 固定部分里某个控件的命中矩形(页面坐标)。
    pub fn page_hit(&self, id: Id) -> Option<Rect> {
        self.page_hits
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, rect)| *rect)
    }

    /// 卡片里某个控件的命中矩形(卡片坐标)。
    pub fn card_hit(&self, id: Id) -> Option<Rect> {
        self.card_hits
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, rect)| *rect)
    }

    /// 卡片最多能往上滚多远(物理像素);内容不够一屏时是 0。
    pub fn max_scroll(&self) -> f32 {
        (self.card.bottom - self.scroll.bottom).max(0.0)
    }
}

/// 后画的控件优先命中:分段格压在组框上。
fn pick(hits: &[(Id, Rect)], x: f32, y: f32) -> Option<Id> {
    hits.iter()
        .rev()
        .find(|(_, rect)| inside(*rect, x, y))
        .map(|(id, _)| *id)
}

/// 点是否落在矩形里(左闭右开)。
fn inside(rect: Rect, x: f32, y: f32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

/// 文字在左还是居中。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Align {
    Left,
    Center,
}

/// 算一页的几何。`scale` 是 DPI 缩放,字型按逻辑字号给;`viewport` 是
/// 窗口客户区高度(物理像素),页脚贴底、卡片在中间滚动。
pub fn layout(page: &Page, font: &Font, scale: f32, viewport: f32) -> Layout {
    let mut out = Layout::default();
    let pad = PAD * scale;
    let gap = GAP * scale;
    let width = PAGE_WIDTH * scale;
    // 头部
    let mut y = pad;
    out.logo = Rect::from_xywh(pad, y, LOGO * scale, LOGO * scale);
    if let Some(button) = &page.header_button {
        let w = button_width(button, font, scale);
        let rect = Rect::from_xywh(
            width - pad - w,
            y + (out.logo.height() - BUTTON_H * scale) / 2.0,
            w,
            BUTTON_H * scale,
        );
        out.page_hits.push((button.id, rect));
        out.page_rings.push((button.id, rect));
    }
    y += out.logo.height() + gap;
    let card_top = y;
    let footer_h = BUTTON_H * scale;
    let footer_top = (viewport - footer_h - pad).max(card_top + footer_h);
    // 左栏:高度从卡片顶到页脚上方,导航项自上而下。
    let sidebar_h = (footer_top - gap - card_top).max(0.0);
    out.sidebar = Rect::from_xywh(pad, card_top, SIDEBAR_W * scale, sidebar_h);
    let item_w = (SIDEBAR_W - SIDEBAR_PAD * 2.0) * scale;
    let mut ny = card_top + SIDEBAR_PAD * scale;
    for item in &page.nav {
        let rect = Rect::from_xywh(pad + SIDEBAR_PAD * scale, ny, item_w, NAV_H * scale);
        out.page_hits.push((item.id, rect));
        out.page_rings.push((item.id, rect));
        ny += (NAV_H + NAV_GAP) * scale;
    }
    // 卡片:内容按卡片坐标排,整体在页面里往下滚。
    let card_left = pad + (SIDEBAR_W + SIDEBAR_GAP) * scale;
    let content = width - pad - card_left;
    let mut cy = scale;
    let last_group = page.groups.len().saturating_sub(1);
    for (gi, group) in page.groups.iter().enumerate() {
        if gi > 0 {
            cy += scale;
        }
        cy += SECTION_TOP * scale;
        cy += SECTION_LINE * scale + SECTION_BOTTOM * scale;
        let last_row = group.rows.len().saturating_sub(1);
        for (ri, row) in group.rows.iter().enumerate() {
            let row_top = cy;
            place_control(&mut out, &row.control, content, row_top, font, scale);
            cy += ROW_H * scale;
            if !(gi == last_group && ri == last_row) {
                cy += scale;
            }
        }
    }
    out.card = Rect::from_xywh(card_left, card_top, content, cy);
    // 页脚贴视口底,滚动区是卡片起点到页脚之间那一条。
    out.footer = Rect::from_xywh(card_left, footer_top, content, footer_h);
    out.scroll = Rect::from_xywh(
        card_left,
        card_top,
        content,
        (footer_top - gap - card_top).max(0.0),
    );
    out.height = out.card.bottom + gap + footer_h + pad;
    // 页脚:提示在左,按钮在右。
    let mut fx = width - pad;
    for button in page.footer_buttons.iter().rev() {
        let w = button_width(button, font, scale);
        fx -= w;
        let rect = Rect::from_xywh(fx, footer_top, w, footer_h);
        out.page_hits.push((button.id, rect));
        out.page_rings.push((button.id, rect));
        fx -= BUTTON_GAP * scale;
    }
    out
}

/// 把行右侧的控件摆到行右边,登记命中区与焦点环。
fn place_control(
    out: &mut Layout,
    control: &Control,
    content: f32,
    row_top: f32,
    font: &Font,
    scale: f32,
) {
    let right = content - ROW_PAD * scale;
    match control {
        Control::Toggle { id, .. } => {
            let rect = Rect::from_xywh(
                right - TOGGLE_W * scale,
                row_top + (ROW_H - TOGGLE_H) * scale / 2.0,
                TOGGLE_W * scale,
                TOGGLE_H * scale,
            );
            out.card_hits.push((*id, rect));
            out.card_rings.push((*id, rect));
        }
        Control::Segment {
            id, options, width, ..
        } => {
            let cell = width * scale;
            let count = options.len().max(1) as f32;
            let group_w =
                SEGMENT_PAD * 2.0 * scale + count * cell + (count - 1.0) * SEGMENT_GAP * scale;
            let group_h = (SEGMENT_H + SEGMENT_PAD * 2.0) * scale;
            let gx = right - group_w;
            let gy = row_top + (ROW_H * scale - group_h) / 2.0;
            out.card_rings
                .push((*id, Rect::from_xywh(gx, gy, group_w, group_h)));
            for index in 0..options.len() {
                let cx = gx + (SEGMENT_PAD + index as f32 * (cell + SEGMENT_GAP)) * scale;
                out.card_hits.push((
                    Id::new(id.base, index),
                    Rect::from_xywh(cx, gy + SEGMENT_PAD * scale, cell, SEGMENT_H * scale),
                ));
            }
        }
        Control::Buttons { items } => {
            let widths: Vec<f32> = items
                .iter()
                .map(|button| button_width(button, font, scale))
                .collect();
            let total: f32 = widths.iter().sum::<f32>()
                + BUTTON_GAP * scale * items.len().saturating_sub(1) as f32;
            let mut x = right - total;
            for (button, w) in items.iter().zip(widths) {
                let rect = Rect::from_xywh(
                    x,
                    row_top + (ROW_H - BUTTON_H) * scale / 2.0,
                    w,
                    BUTTON_H * scale,
                );
                out.card_hits.push((button.id, rect));
                out.card_rings.push((button.id, rect));
                x += w + BUTTON_GAP * scale;
            }
        }
        Control::ButtonsToggle {
            items,
            toggle,
            toggle_on: _,
        } => {
            // 开关贴行右,按钮排在它左边。
            let toggle_rect = Rect::from_xywh(
                right - TOGGLE_W * scale,
                row_top + (ROW_H - TOGGLE_H) * scale / 2.0,
                TOGGLE_W * scale,
                TOGGLE_H * scale,
            );
            out.card_hits.push((*toggle, toggle_rect));
            out.card_rings.push((*toggle, toggle_rect));
            let widths: Vec<f32> = items
                .iter()
                .map(|button| button_width(button, font, scale))
                .collect();
            let total: f32 = widths.iter().sum::<f32>()
                + BUTTON_GAP * scale * items.len().saturating_sub(1) as f32;
            let mut x = toggle_rect.left - BUTTON_GAP * scale - total;
            for (button, w) in items.iter().zip(widths) {
                let rect = Rect::from_xywh(
                    x,
                    row_top + (ROW_H - BUTTON_H) * scale / 2.0,
                    w,
                    BUTTON_H * scale,
                );
                out.card_hits.push((button.id, rect));
                out.card_rings.push((button.id, rect));
                x += w + BUTTON_GAP * scale;
            }
        }
        Control::None => {}
    }
}

/// 按钮宽度:文字宽 + 左右留白。
fn button_width(button: &Button, font: &Font, scale: f32) -> f32 {
    let face = face(font, BUTTON_TEXT, false, scale);
    face.measure_str(&button.text, None).0 + BUTTON_PAD * 2.0 * scale
}

/// 按钮文字字号。
const BUTTON_TEXT: f32 = 12.5;

/// 按字号与字重取一份字型。
fn face(font: &Font, size: f32, bold: bool, scale: f32) -> Font {
    let mut face = font.clone();
    face.set_size(size * scale);
    face.set_embolden(bold);
    face
}

/// 按可用宽度截断文字,末尾补省略号。
fn ellipsize(face: &Font, text: &str, max_width: f32) -> String {
    if face.measure_str(text, None).0 <= max_width {
        return text.to_string();
    }
    let mut out = text.to_string();
    while !out.is_empty() {
        let mut candidate = out.clone();
        candidate.push('…');
        if face.measure_str(&candidate, None).0 <= max_width {
            return candidate;
        }
        out.pop();
    }
    "…".to_string()
}
