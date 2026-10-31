//! 宿主选择面板绘制:标题、条目列表、页脚与命中。
//!
//! 只认数据:标题与可见行由服务端的 `HostPanel` 模型给出,调色板与字型
//! 由窗口层传入。绘制与命中共用同一份 [`Layout`],所指即所画。

use skia_safe::{Canvas, Contains, Font, Paint, PaintStyle, Point, RRect, Rect};

use crate::command::{ellipsize, fill, text};
use crate::panel::{Palette, color};

/// 面板逻辑宽度。
pub const WIDTH: f32 = 440.0;
/// 标题区高。
pub const TITLE_H: f32 = 52.0;
/// 单行条目高。
pub const ROW_H: f32 = 36.0;
/// 页脚高。
pub const FOOTER_H: f32 = 28.0;
/// 一屏最多显示多少条。
pub const MAX_ROWS: usize = 8;
/// 左右内边距。
const PAD: f32 = 16.0;
/// 行内圆角。
const RADIUS: f32 = 6.0;

/// 一帧的输入数据。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    /// 面板标题。
    pub title: String,
    /// 当前可见行。
    pub rows: Vec<String>,
    /// 选中行在 `rows` 里的下标。
    pub selected: usize,
    /// 条目总数。
    pub total: usize,
    /// 可见区上方被卷掉的行数。
    pub above: usize,
    /// 可见区下方还有多少行。
    pub below: usize,
}

/// 绘制与命中共用的几何(物理像素)。
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub size: (f32, f32),
    pub title: Rect,
    pub list: Rect,
    pub footer: Rect,
    pub rows: Vec<Rect>,
}

/// 鼠标命中区域。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Row(usize),
    Nothing,
}

/// 按可见行数算尺寸与行矩形;没有行时留一块空状态区。
pub fn layout(spec: &Spec, scale: f32) -> Layout {
    let scale = scale.max(0.5);
    let row_count = spec.rows.len().max(1) as f32;
    let width = WIDTH * scale;
    let height = (TITLE_H + row_count * ROW_H + FOOTER_H) * scale;
    let title = Rect::from_xywh(0.0, 0.0, width, TITLE_H * scale);
    let list = Rect::from_xywh(0.0, TITLE_H * scale, width, row_count * ROW_H * scale);
    let footer = Rect::from_xywh(0.0, height - FOOTER_H * scale, width, FOOTER_H * scale);
    let rows = (0..spec.rows.len())
        .map(|index| {
            Rect::from_xywh(
                PAD * scale,
                list.top + index as f32 * ROW_H * scale,
                width - 2.0 * PAD * scale,
                ROW_H * scale,
            )
        })
        .collect();
    Layout {
        size: (width, height),
        title,
        list,
        footer,
        rows,
    }
}

/// 画一帧;调用方按 [`Layout::size`] 准备同样大的画布。
pub fn draw(
    canvas: &Canvas,
    spec: &Spec,
    layout: &Layout,
    palette: &Palette,
    font: &Font,
    scale: f32,
) {
    let scale = scale.max(0.5);
    let (width, height) = layout.size;
    canvas.draw_rect(Rect::from_xywh(0.0, 0.0, width, height), &fill(palette.bg));
    let mut border = Paint::default();
    border.set_anti_alias(true);
    border.set_style(PaintStyle::Stroke);
    border.set_stroke_width(scale);
    border.set_color(color(palette.border));
    canvas.draw_rect(
        Rect::from_xywh(0.5 * scale, 0.5 * scale, width - scale, height - scale),
        &border,
    );

    draw_title(canvas, spec, layout, palette, font, scale);
    draw_rows(canvas, spec, layout, palette, font, scale);
    draw_footer(canvas, spec, layout, palette, font, scale);
}

fn draw_title(
    canvas: &Canvas,
    spec: &Spec,
    layout: &Layout,
    palette: &Palette,
    font: &Font,
    scale: f32,
) {
    let size = 15.0 * scale;
    let baseline = layout.title.center_y() + size * 0.36;
    let count = format!("{} 项", spec.total);
    let mut count_font = font.clone();
    count_font.set_size(11.0 * scale);
    let (count_width, _) = count_font.measure_str(&count, None);
    let title_width = (layout.title.width() - 3.0 * PAD * scale - count_width).max(0.0);
    let title = ellipsize(font, size, &spec.title, title_width);
    text(
        canvas,
        font,
        size,
        palette.text,
        Point::new(PAD * scale, baseline),
        &title,
        Some((title_width, size)),
    );
    text(
        canvas,
        font,
        11.0 * scale,
        palette.text_tertiary,
        Point::new(
            layout.title.right - PAD * scale - count_width,
            layout.title.center_y() + 11.0 * scale * 0.36,
        ),
        &count,
        None,
    );
    canvas.draw_rect(
        Rect::from_xywh(
            PAD * scale,
            layout.title.bottom - scale,
            layout.title.width() - 2.0 * PAD * scale,
            scale,
        ),
        &fill(palette.border),
    );
}

fn draw_rows(
    canvas: &Canvas,
    spec: &Spec,
    layout: &Layout,
    palette: &Palette,
    font: &Font,
    scale: f32,
) {
    if spec.rows.is_empty() {
        let size = 14.0 * scale;
        let mut message_font = font.clone();
        message_font.set_size(size);
        let (width, _) = message_font.measure_str("没有可选项", None);
        text(
            canvas,
            font,
            size,
            palette.text_secondary,
            Point::new(
                layout.list.center_x() - width * 0.5,
                layout.list.center_y() + size * 0.36,
            ),
            "没有可选项",
            None,
        );
        return;
    }
    let size = 14.0 * scale;
    for (index, row) in spec.rows.iter().enumerate() {
        let Some(rect) = layout.rows.get(index) else {
            continue;
        };
        let selected = index == spec.selected;
        if selected {
            let inner = Rect::from_ltrb(
                rect.left,
                rect.top + 2.0 * scale,
                rect.right,
                rect.bottom - 2.0 * scale,
            );
            canvas.draw_rrect(
                RRect::new_rect_xy(inner, RADIUS * scale, RADIUS * scale),
                &fill(palette.selected_bg),
            );
        }
        let max_width = rect.width() - 2.0 * PAD * scale;
        let shown = ellipsize(font, size, row, max_width);
        text(
            canvas,
            font,
            size,
            if selected {
                palette.selected_text
            } else {
                palette.text
            },
            Point::new(
                rect.left + PAD * scale,
                rect.center_y() + size * 0.36,
            ),
            &shown,
            Some((max_width, size)),
        );
    }
    // 卷动提示:上下还有内容时在列表右缘画小箭头。
    let mut arrow_font = font.clone();
    arrow_font.set_size(10.0 * scale);
    let mut arrow = Paint::default();
    arrow.set_anti_alias(true);
    arrow.set_color(color(palette.text_tertiary));
    if spec.above > 0 {
        canvas.draw_str(
            "▲",
            Point::new(
                layout.list.right - 12.0 * scale,
                layout.list.top + 9.0 * scale,
            ),
            &arrow_font,
            &arrow,
        );
    }
    if spec.below > 0 {
        canvas.draw_str(
            "▼",
            Point::new(
                layout.list.right - 12.0 * scale,
                layout.list.bottom - 3.0 * scale,
            ),
            &arrow_font,
            &arrow,
        );
    }
}

fn draw_footer(
    canvas: &Canvas,
    spec: &Spec,
    layout: &Layout,
    palette: &Palette,
    font: &Font,
    scale: f32,
) {
    let size = 11.0 * scale;
    let baseline = layout.footer.center_y() + size * 0.36;
    text(
        canvas,
        font,
        size,
        palette.text_tertiary,
        Point::new(PAD * scale, baseline),
        "↑↓ 选择 · Enter 确认 · Esc 取消",
        Some((layout.footer.width() - 2.0 * PAD * scale, size)),
    );
    if spec.total == 0 {
        return;
    }
    let label = format!("{}/{}", spec.above + spec.selected + 1, spec.total);
    let mut label_font = font.clone();
    label_font.set_size(size);
    let (label_width, _) = label_font.measure_str(&label, None);
    text(
        canvas,
        font,
        size,
        palette.text_tertiary,
        Point::new(layout.footer.right - PAD * scale - label_width, baseline),
        &label,
        None,
    );
}

/// 鼠标命中;坐标是窗口客户区物理像素。
pub fn hit(layout: &Layout, spec: &Spec, x: f32, y: f32) -> Hit {
    for (index, rect) in layout.rows.iter().enumerate() {
        if rect.contains(Point::new(x, y)) && index < spec.rows.len() {
            return Hit::Row(index);
        }
    }
    Hit::Nothing
}
