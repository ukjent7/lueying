//! 设置窗控件的绘制:行、开关、分段、按钮与页脚的 Skia 出帧。
//!
//! `ui` 的 `#[path]` 子模块(src 保持扁平);布局与命中留在父模块,
//! 共享的几何助手经 `super::*` 取用。

use skia_safe::{Canvas, ClipOp, Paint, PaintStyle, Point, RRect, Rect, Vector};

use crate::panel::{Palette, color, fill_paint};

use super::*;

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
        self.sidebar(page, layout, focus);
        // 卡片:裁在滚动区里,内容按滚动量上移。
        self.canvas.save();
        self.canvas
            .clip_rect(layout.scroll, ClipOp::Intersect, false);
        self.canvas
            .translate((layout.card.left, layout.card.top - scroll));
        self.card(layout);
        self.groups(page, layout, focus);
        self.canvas.restore();
        self.scrollbar(layout, scroll);
        self.footer(page, layout);
    }

    /// 左栏:一块浅色面板,当前页用主色文字和淡色选中底。
    fn sidebar(&self, page: &Page, layout: &Layout, focus: Option<Id>) {
        let s = self.scale;
        let panel = layout.sidebar;
        if panel.width() <= 0.0 || panel.height() <= 0.0 {
            return;
        }
        self.canvas.draw_rrect(
            RRect::new_rect_radii(panel, &[Vector::new(CARD_RADIUS * s, CARD_RADIUS * s); 4]),
            &fill_paint(self.palette.surface_alt),
        );
        for (index, item) in page.nav.iter().enumerate() {
            let Some(rect) = layout.ring_page(item.id) else {
                continue;
            };
            let selected = index == page.active_nav;
            if selected {
                self.canvas.draw_rrect(
                    RRect::new_rect_radii(rect, &[Vector::new(RADIUS * s, RADIUS * s); 4]),
                    &fill_paint(self.palette.selected_bg),
                );
                let bar = Rect::from_xywh(
                    rect.left + 4.0 * s,
                    rect.top + 8.0 * s,
                    3.0 * s,
                    rect.height() - 16.0 * s,
                );
                self.canvas.draw_rrect(
                    RRect::new_rect_radii(bar, &[Vector::new(1.5 * s, 1.5 * s); 4]),
                    &fill_paint(self.palette.accent),
                );
            }
            let text = Rect::from_xywh(
                rect.left + 14.0 * s,
                rect.top,
                rect.width() - 20.0 * s,
                rect.height(),
            );
            self.label_clipped(
                &item.text,
                12.5,
                selected,
                if selected {
                    self.palette.accent_text
                } else {
                    self.palette.text_secondary
                },
                text,
                Align::Left,
            );
            if focus == Some(item.id) {
                self.focus_ring(rect);
            }
        }
    }

    /// 滚动条:内容比视口高时,右边画一根细条,滑块按滚动量走。
    fn scrollbar(&self, layout: &Layout, scroll: f32) {
        let max = layout.max_scroll();
        if max <= 0.0 {
            return;
        }
        let s = self.scale;
        let track_h = layout.scroll.height();
        if track_h <= 0.0 {
            return;
        }
        // 滑块长度 = 可见比例,至少留一截把手。
        let ratio = (layout.scroll.height() / layout.card.height()).clamp(0.08, 1.0);
        let thumb_h = (track_h * ratio).max(28.0 * s);
        let travel = (track_h - thumb_h).max(0.0);
        let pos = (scroll / max).clamp(0.0, 1.0) * travel;
        let rect = Rect::from_xywh(
            layout.scroll.right - 7.0 * s,
            layout.scroll.top + pos,
            3.0 * s,
            thumb_h,
        );
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        paint.set_color(color(self.palette.border_strong));
        self.canvas.draw_rrect(
            RRect::new_rect_radii(rect, &[Vector::new(1.5 * s, 1.5 * s); 4]),
            &paint,
        );
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
                    // 选中格可以用自己的颜色(主题色一列),没选中的走次级色。
                    let tint = if index == *selected {
                        option.tint.unwrap_or(self.palette.text)
                    } else {
                        self.palette.text_secondary
                    };
                    self.label_clipped(
                        &option.text,
                        12.0,
                        index == *selected,
                        tint,
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
            Control::ButtonsToggle {
                items,
                toggle,
                toggle_on,
            } => {
                for button in items {
                    if let Some(rect) = layout.ring_card(button.id) {
                        self.button(button, rect);
                    }
                }
                if let Some(rect) = layout.ring_card(*toggle) {
                    self.toggle(*toggle_on, rect);
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
        let top = layout.footer.top;
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
