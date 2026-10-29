//! 设置窗的控件层:页面描述、布局、绘制与命中。
//!
//! 设置窗的结构是「一页说明 + 每行一个控件」,不需要通用 GUI 框架:
//! [`Page`] 描述内容,[`layout`] 算几何,[`Painter`] 出帧,[`Layout::hit`]
//! 把点击反查回控件。平台侧只管窗口、事件与状态,画面与命中都在这里,
//! Linux E2E 因此能直接断言布局与像素。

use skia_safe::{Canvas, ClipOp, Font, Paint, PaintStyle, Point, RRect, Rect, Vector};

use crate::panel::{Palette, color, fill_paint};

/// 页面宽度(逻辑像素)。
pub const PAGE_WIDTH: f32 = 460.0;
/// 页面留白。
const PAD: f32 = 18.0;
/// 头部、卡片与页脚之间的间距。
const GAP: f32 = 10.0;
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

/// 行右侧的控件。
#[derive(Clone, Debug)]
pub enum Control {
    /// 开关。
    Toggle { id: Id, on: bool },
    /// 分段组;每格按下标有自己的 id,`selected` 是选中格。
    Segment {
        id: Id,
        options: Vec<String>,
        selected: usize,
        /// 每格宽度(逻辑像素)。
        width: f32,
    },
    /// 一排按钮。
    Buttons { items: Vec<Button> },
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

/// 页面:头部、分组与页脚。
#[derive(Clone, Debug)]
pub struct Page {
    pub title: String,
    pub subtitle: String,
    /// 头部右侧的按钮(检查更新)。
    pub header_button: Option<Button>,
    pub groups: Vec<Group>,
    /// 页脚提示文字与颜色。
    pub notice: String,
    pub notice_tint: u32,
    /// 页脚右侧的按钮(关闭、保存)。
    pub footer_buttons: Vec<Button>,
}

/// 布局结果:几何都在物理像素里。
#[derive(Clone, Debug, Default)]
pub struct Layout {
    /// 内容总高(物理像素)。
    pub height: f32,
    /// 卡片在页面里的位置。
    pub card: Rect,
    /// 滚动区(页面坐标);卡片在里面滚。
    pub scroll: Rect,
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

/// 算一页的几何。`scale` 是 DPI 缩放,字型按逻辑字号给。
pub fn layout(page: &Page, font: &Font, scale: f32) -> Layout {
    let mut out = Layout::default();
    let pad = PAD * scale;
    let gap = GAP * scale;
    let width = PAGE_WIDTH * scale;
    let content = width - pad * 2.0;
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
    // 卡片:内容按卡片坐标排,整体在页面里往下滚。
    let card_top = y;
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
    out.card = Rect::from_xywh(pad, card_top, content, cy);
    // 滚动区:卡片之外留给页脚的高度。
    let footer_h = BUTTON_H * scale;
    out.height = out.card.bottom + gap + footer_h + pad;
    out.scroll = Rect::from_xywh(
        pad,
        card_top,
        content,
        (out.height - footer_h - pad * 2.0 - gap).max(0.0),
    );
    // 页脚:提示在左,按钮在右。
    let footer_top = out.card.bottom + gap;
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

/// 绘制上下文:画布、配色、字型与缩放。
pub struct Painter<'a> {
    canvas: &'a Canvas,
    palette: &'a Palette,
    font: &'a Font,
    scale: f32,
}

impl<'a> Painter<'a> {
    pub fn new(canvas: &'a Canvas, palette: &'a Palette, font: &'a Font, scale: f32) -> Self {
        Self {
            canvas,
            palette,
            font,
            scale,
        }
    }

    /// 画整页。`scroll` 是卡片往上滚过的距离(物理像素)。
    pub fn paint(&self, page: &Page, layout: &Layout, focus: Option<Id>, scroll: f32) {
        self.canvas.clear(color(self.palette.window_bg));
        self.header(page, layout);
        // 卡片:裁在滚动区里,内容按滚动量上移。
        self.canvas.save();
        self.canvas
            .clip_rect(layout.scroll, ClipOp::Intersect, false);
        self.canvas
            .translate((layout.card.left, layout.card.top - scroll));
        self.card(layout);
        self.groups(page, layout, focus);
        self.canvas.restore();
        self.footer(page, layout);
    }

    /// 品牌、标题与头部按钮。
    fn header(&self, page: &Page, layout: &Layout) {
        let s = self.scale;
        let logo = layout.logo;
        self.canvas.draw_rrect(
            RRect::new_rect_radii(logo, &[Vector::new(LOGO_RADIUS * s, LOGO_RADIUS * s); 4]),
            &fill_paint(self.palette.accent),
        );
        self.label(
            "掠",
            18.0,
            true,
            self.palette.on_accent,
            logo,
            Align::Center,
        );
        let text_left = logo.right + 12.0 * s;
        let title = Rect::from_xywh(text_left, logo.top, 230.0 * s, 22.0 * s);
        self.label(
            &page.title,
            19.0,
            true,
            self.palette.text,
            title,
            Align::Left,
        );
        let subtitle = Rect::from_xywh(text_left, logo.top + 23.0 * s, 230.0 * s, 15.0 * s);
        self.label_clipped(
            &page.subtitle,
            12.0,
            false,
            self.palette.text_secondary,
            subtitle,
            Align::Left,
        );
        if let Some(button) = &page.header_button
            && let Some(rect) = layout.ring_page(button.id)
        {
            self.button(button, rect);
        }
    }

    /// 卡片底:圆角矩形加一圈描边。画在卡片坐标里。
    fn card(&self, layout: &Layout) {
        let s = self.scale;
        let card = Rect::from_xywh(0.0, 0.0, layout.card.width(), layout.card.height());
        self.canvas.draw_rrect(
            RRect::new_rect_radii(card, &[Vector::new(CARD_RADIUS * s, CARD_RADIUS * s); 4]),
            &fill_paint(self.palette.bg),
        );
        let mut stroke = Paint::default();
        stroke.set_anti_alias(true);
        stroke.set_style(PaintStyle::Stroke);
        stroke.set_stroke_width(s);
        stroke.set_color(color(self.palette.border));
        self.canvas.draw_rrect(
            RRect::new_rect_radii(card, &[Vector::new(CARD_RADIUS * s, CARD_RADIUS * s); 4]),
            &stroke,
        );
    }

    /// 分组标题、设置行与行内控件。
    fn groups(&self, page: &Page, layout: &Layout, focus: Option<Id>) {
        let s = self.scale;
        let mut y = s;
        let last_group = page.groups.len().saturating_sub(1);
        for (gi, group) in page.groups.iter().enumerate() {
            if gi > 0 {
                self.separator(y, layout.card.width());
                y += s;
            }
            y += SECTION_TOP * s;
            self.label(
                &group.title,
                SECTION_SIZE,
                true,
                self.palette.text_tertiary,
                Rect::from_xywh(ROW_PAD * s, y, layout.card.width(), SECTION_LINE * s),
                Align::Left,
            );
            y += SECTION_LINE * s + SECTION_BOTTOM * s;
            let last_row = group.rows.len().saturating_sub(1);
            for (ri, row) in group.rows.iter().enumerate() {
                let row_top = y;
                self.row(row, row_top, layout);
                y += ROW_H * s;
                if !(gi == last_group && ri == last_row) {
                    self.separator(y, layout.card.width());
                    y += s;
                }
            }
        }
        if let Some(id) = focus
            && let Some(rect) = layout.ring_card(id)
        {
            self.focus_ring(rect);
        }
    }

    /// 一行:标题、说明与右侧控件。
    fn row(&self, row: &Row, row_top: f32, layout: &Layout) {
        let s = self.scale;
        let left = ROW_PAD * s;
        let label = Rect::from_xywh(left, row_top + 3.0 * s, DETAIL_MAX * s, 17.0 * s);
        self.label(
            &row.label,
            LABEL_SIZE,
            false,
            self.palette.text,
            label,
            Align::Left,
        );
        let detail = Rect::from_xywh(left, row_top + 21.0 * s, DETAIL_MAX * s, 15.0 * s);
        self.label_clipped(
            &row.detail,
            DETAIL_SIZE,
            false,
            self.palette.text_secondary,
            detail,
            Align::Left,
        );
        match &row.control {
            Control::Toggle { id, on } => {
                if let Some(rect) = layout.ring_card(*id) {
                    self.toggle(*on, rect);
                }
            }
            Control::Segment {
                id,
                options,
                selected,
                ..
            } => {
                let group = layout.ring_card(*id);
                let Some(group) = group else {
                    return;
                };
                self.segment_group(group);
                let count = options.len().max(1) as f32;
                // 格子宽度从组框反推:组框 = 内边距 + 格子 + 格间空隙。
                let cell_w =
                    (group.width() - SEGMENT_PAD * 2.0 * s - (count - 1.0) * SEGMENT_GAP * s)
                        / count;
                for (index, option) in options.iter().enumerate() {
                    let rect = Rect::from_xywh(
                        group.left + SEGMENT_PAD * s + index as f32 * (cell_w + SEGMENT_GAP * s),
                        group.top + SEGMENT_PAD * s,
                        cell_w,
                        SEGMENT_H * s,
                    );
                    if index == *selected {
                        self.canvas.draw_rrect(
                            RRect::new_rect_radii(rect, &[Vector::new(RADIUS * s, RADIUS * s); 4]),
                            &fill_paint(self.palette.segment),
                        );
                    }
                    self.label_clipped(
                        option,
                        12.0,
                        index == *selected,
                        if index == *selected {
                            self.palette.text
                        } else {
                            self.palette.text_secondary
                        },
                        rect,
                        Align::Center,
                    );
                }
            }
            Control::Buttons { items } => {
                for button in items {
                    if let Some(rect) = layout.ring_card(button.id) {
                        self.button(button, rect);
                    }
                }
            }
            Control::None => {}
        }
    }

    /// 分组之间与行之间的分隔线。
    fn separator(&self, y: f32, width: f32) {
        let s = self.scale;
        let mut paint = Paint::default();
        paint.set_anti_alias(false);
        paint.set_color(color(self.palette.border));
        self.canvas
            .draw_rect(Rect::from_xywh(0.0, y, width, s), &paint);
    }

    /// 页脚:提示文字与按钮。
    fn footer(&self, page: &Page, layout: &Layout) {
        let s = self.scale;
        let top = layout.card.bottom + GAP * s;
        if !page.notice.is_empty() {
            let dot = Rect::from_xywh(
                ROW_PAD * s,
                top + (BUTTON_H * s - 6.0 * s) / 2.0,
                6.0 * s,
                6.0 * s,
            );
            self.canvas.draw_rrect(
                RRect::new_rect_radii(dot, &[Vector::new(3.0 * s, 3.0 * s); 4]),
                &fill_paint(page.notice_tint),
            );
        }
        let text = Rect::from_xywh(ROW_PAD * s + 14.0 * s, top, 280.0 * s, BUTTON_H * s);
        self.label_clipped(
            &page.notice,
            12.0,
            false,
            page.notice_tint,
            text,
            Align::Left,
        );
        for button in &page.footer_buttons {
            if let Some(rect) = layout.ring_page(button.id) {
                self.button(button, rect);
            }
        }
    }

    /// 开关:轨道加旋钮。
    fn toggle(&self, on: bool, rect: Rect) {
        let s = self.scale;
        let track = if on {
            self.palette.accent
        } else {
            self.palette.off_bg
        };
        self.canvas.draw_rrect(
            RRect::new_rect_radii(
                rect,
                &[Vector::new(TOGGLE_H * s / 2.0, TOGGLE_H * s / 2.0); 4],
            ),
            &fill_paint(track),
        );
        let knob = KNOB * s;
        let knob_x = if on {
            rect.right - knob - 3.0 * s
        } else {
            rect.left + 3.0 * s
        };
        let knob_rect = Rect::from_xywh(knob_x, rect.top + 3.0 * s, knob, knob);
        self.canvas.draw_rrect(
            RRect::new_rect_radii(knob_rect, &[Vector::new(knob, knob); 4]),
            &fill_paint(if on {
                self.palette.on_accent
            } else {
                self.palette.knob_off
            }),
        );
    }

    /// 分段组的外框。
    fn segment_group(&self, rect: Rect) {
        let s = self.scale;
        self.canvas.draw_rrect(
            RRect::new_rect_radii(rect, &[Vector::new(RADIUS * s, RADIUS * s); 4]),
            &fill_paint(self.palette.surface_alt),
        );
        let mut stroke = Paint::default();
        stroke.set_anti_alias(true);
        stroke.set_style(PaintStyle::Stroke);
        stroke.set_stroke_width(s);
        stroke.set_color(color(self.palette.border));
        self.canvas.draw_rrect(
            RRect::new_rect_radii(rect, &[Vector::new(RADIUS * s, RADIUS * s); 4]),
            &stroke,
        );
    }

    /// 按钮:按外观取底色与文字色。
    fn button(&self, button: &Button, rect: Rect) {
        let s = self.scale;
        let (bg, text) = match button.kind {
            ButtonKind::Primary => (Some(self.palette.accent), self.palette.on_accent),
            ButtonKind::Secondary => (Some(self.palette.bg), self.palette.text),
            ButtonKind::Ghost => (None, self.palette.text_secondary),
            ButtonKind::Danger => (Some(self.palette.danger_soft), self.palette.danger),
        };
        if let Some(bg) = bg {
            self.canvas.draw_rrect(
                RRect::new_rect_radii(rect, &[Vector::new(RADIUS * s, RADIUS * s); 4]),
                &fill_paint(bg),
            );
        }
        if button.kind == ButtonKind::Secondary {
            let mut stroke = Paint::default();
            stroke.set_anti_alias(true);
            stroke.set_style(PaintStyle::Stroke);
            stroke.set_stroke_width(s);
            stroke.set_color(color(self.palette.border_strong));
            self.canvas.draw_rrect(
                RRect::new_rect_radii(rect, &[Vector::new(RADIUS * s, RADIUS * s); 4]),
                &stroke,
            );
        }
        self.label(
            &button.text,
            BUTTON_TEXT,
            false,
            if button.enabled {
                text
            } else {
                self.palette.text_tertiary
            },
            rect,
            Align::Center,
        );
    }

    /// 焦点环:控件外一圈主色。
    fn focus_ring(&self, rect: Rect) {
        let s = self.scale;
        let mut stroke = Paint::default();
        stroke.set_anti_alias(true);
        stroke.set_style(PaintStyle::Stroke);
        stroke.set_stroke_width(2.0 * s);
        stroke.set_color(color(self.palette.accent));
        self.canvas.draw_rrect(
            RRect::new_rect_radii(
                Rect::from_xywh(
                    rect.left - s,
                    rect.top - s,
                    rect.width() + 2.0 * s,
                    rect.height() + 2.0 * s,
                ),
                &[Vector::new(RADIUS * s, RADIUS * s); 4],
            ),
            &stroke,
        );
    }

    /// 画一行字,按对齐方式摆进矩形。
    fn label(&self, content: &str, size: f32, bold: bool, rgb: u32, rect: Rect, align: Align) {
        let face = face(self.font, size, bold, self.scale);
        let width = face.measure_str(content, None).0;
        let (_, metrics) = face.metrics();
        let x = match align {
            Align::Left => rect.left,
            Align::Center => rect.left + (rect.width() - width) / 2.0,
        };
        let baseline = (rect.top + rect.bottom) / 2.0 - (metrics.ascent + metrics.descent) / 2.0;
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(color(rgb));
        self.canvas
            .draw_str(content, Point::new(x, baseline), &face, &paint);
    }

    /// 画一行字;超宽按宽度截断并补省略号。
    fn label_clipped(
        &self,
        content: &str,
        size: f32,
        bold: bool,
        rgb: u32,
        rect: Rect,
        align: Align,
    ) {
        let face = face(self.font, size, bold, self.scale);
        let text = ellipsize(&face, content, rect.width());
        let width = face.measure_str(&text, None).0;
        let (_, metrics) = face.metrics();
        let x = match align {
            Align::Left => rect.left,
            Align::Center => rect.left + (rect.width() - width) / 2.0,
        };
        let baseline = (rect.top + rect.bottom) / 2.0 - (metrics.ascent + metrics.descent) / 2.0;
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(color(rgb));
        self.canvas.save();
        self.canvas.clip_rect(rect, ClipOp::Intersect, false);
        self.canvas
            .draw_str(&text, Point::new(x, baseline), &face, &paint);
        self.canvas.restore();
    }
}

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
