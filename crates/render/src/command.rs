//! 命令面板绘制:输入框、竖排命令列表、页脚与命中。
//!
//! 只认数据:查询串、可见行与选中下标由服务端的命令搜索模型给出,调色板
//! 与字型由窗口层传入。绘制与命中共用同一份 [`Layout`],所指即所画。

use skia_safe::{Canvas, Contains, Font, Paint, PaintStyle, Point, RRect, Rect};

use crate::panel::{Palette, color};

/// 面板逻辑宽度。
pub const WIDTH: f32 = 520.0;
/// 左右内边距。
pub const PAD: f32 = 14.0;
/// 输入框高。
pub const INPUT_H: f32 = 46.0;
/// 单行命令高。
pub const ROW_H: f32 = 34.0;
/// 页脚高。
pub const FOOTER_H: f32 = 28.0;
/// 一屏最多显示多少条命令。
pub const MAX_ROWS: usize = 8;
/// 行内圆角。
const RADIUS: f32 = 6.0;

/// 一帧的输入数据。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    /// 输入框里的查询串。
    pub query: String,
    /// 当前可见行。
    pub rows: Vec<Row>,
    /// 选中行在 `rows` 里的下标。
    pub selected: usize,
    /// 匹配总数。
    pub total: usize,
    /// 可见区上方被卷掉的行数。
    pub above: usize,
    /// 可见区下方还有多少行。
    pub below: usize,
}

/// 一条命令行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    pub extension: String,
}

/// 绘制与命中共用的几何(物理像素)。
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub size: (f32, f32),
    pub input: Rect,
    pub list: Rect,
    pub footer: Rect,
    pub rows: Vec<Rect>,
}

/// 鼠标命中区域。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Input,
    Row(usize),
    Footer,
    Nothing,
}

/// 按可见行数算尺寸与行矩形;没有行时留一块空状态区。
pub fn layout(spec: &Spec, scale: f32) -> Layout {
    let scale = scale.max(0.5);
    let row_count = spec.rows.len().max(1) as f32;
    let width = WIDTH * scale;
    let height = (INPUT_H + row_count * ROW_H + FOOTER_H) * scale;
    let input = Rect::from_xywh(0.0, 0.0, width, INPUT_H * scale);
    let list = Rect::from_xywh(0.0, INPUT_H * scale, width, row_count * ROW_H * scale);
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
        input,
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
    // 输入框与列表之间的分隔线。
    canvas.draw_rect(
        Rect::from_xywh(0.0, layout.input.bottom - scale, width, scale),
        &fill(palette.border),
    );
    // 外框;窗口没有系统边框,描边由这里画。
    let mut border = Paint::default();
    border.set_anti_alias(true);
    border.set_style(PaintStyle::Stroke);
    border.set_stroke_width(scale);
    border.set_color(color(palette.border));
    canvas.draw_rect(
        Rect::from_xywh(0.5 * scale, 0.5 * scale, width - scale, height - scale),
        &border,
    );

    draw_input(canvas, spec, layout, palette, font, scale);
    draw_rows(canvas, spec, layout, palette, font, scale);
    draw_footer(canvas, spec, layout, palette, font, scale);
}

fn draw_input(
    canvas: &Canvas,
    spec: &Spec,
    layout: &Layout,
    palette: &Palette,
    font: &Font,
    scale: f32,
) {
    let size = 15.0 * scale;
    let baseline = layout.input.center_y() + size * 0.36;
    let x = PAD * scale;
    let max_width = layout.input.width() - 2.0 * PAD * scale;
    if spec.query.is_empty() {
        text(
            canvas,
            font,
            size,
            palette.text_tertiary,
            Point::new(x, baseline),
            "输入拼音搜索命令",
            Some((max_width, size)),
        );
        // 空查询时把光标画在提示文字前。
        caret(canvas, x, layout.input, palette, scale);
        return;
    }
    let shown = ellipsize(font, size, &spec.query, max_width - 2.0 * scale);
    let mut query_font = font.clone();
    query_font.set_size(size);
    let (query_width, _) = query_font.measure_str(&shown, None);
    text(
        canvas,
        font,
        size,
        palette.text,
        Point::new(x, baseline),
        &shown,
        Some((max_width, size)),
    );
    caret(
        canvas,
        x + query_width + 2.0 * scale,
        layout.input,
        palette,
        scale,
    );
}

fn caret(canvas: &Canvas, x: f32, input: Rect, palette: &Palette, scale: f32) {
    let x = x.min(input.right - PAD * scale);
    let height = 18.0 * scale;
    canvas.draw_rect(
        Rect::from_xywh(x, input.center_y() - height * 0.5, 1.5 * scale, height),
        &fill(palette.accent),
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
        let message = if spec.total == 0 && spec.query.is_empty() {
            "没有可用的命令"
        } else {
            "没有匹配的命令"
        };
        let size = 14.0 * scale;
        let mut message_font = font.clone();
        message_font.set_size(size);
        let (width, _) = message_font.measure_str(message, None);
        let baseline = layout.list.center_y() + size * 0.36;
        text(
            canvas,
            font,
            size,
            palette.text_secondary,
            Point::new(layout.list.center_x() - width * 0.5, baseline),
            message,
            None,
        );
        return;
    }
    let size = 14.0 * scale;
    let small = 11.0 * scale;
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
        let mut small_font = font.clone();
        small_font.set_size(small);
        let (extension_width, _) = small_font.measure_str(&row.extension, None);
        let extension_x = rect.right - PAD * scale - extension_width;
        let title_width = (extension_x - rect.left - PAD * scale - 10.0 * scale).max(0.0);
        let title = ellipsize(font, size, &row.title, title_width);
        let baseline = rect.center_y() + size * 0.36;
        text(
            canvas,
            font,
            size,
            if selected {
                palette.selected_text
            } else {
                palette.text
            },
            Point::new(rect.left + PAD * scale, baseline),
            &title,
            Some((title_width, size)),
        );
        let small_baseline = rect.center_y() + small * 0.36;
        text(
            canvas,
            font,
            small,
            if selected {
                palette.accent_text
            } else {
                palette.text_tertiary
            },
            Point::new(extension_x, small_baseline),
            &row.extension,
            None,
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
                layout.list.right - 10.0 * scale,
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
                layout.list.right - 10.0 * scale,
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
        "↑↓ 选择 · Enter 执行 · Esc 关闭",
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
    if layout.input.contains(Point::new(x, y)) {
        return Hit::Input;
    }
    for (index, rect) in layout.rows.iter().enumerate() {
        if rect.contains(Point::new(x, y)) && index < spec.rows.len() {
            return Hit::Row(index);
        }
    }
    if layout.footer.contains(Point::new(x, y)) {
        return Hit::Footer;
    }
    Hit::Nothing
}

/// 画一段文字;`clip` 给 (最大宽, 字号) 时先裁剪再画。
fn text(
    canvas: &Canvas,
    font: &Font,
    size: f32,
    rgb: u32,
    origin: Point,
    content: &str,
    clip: Option<(f32, f32)>,
) {
    let (x, baseline) = (origin.x, origin.y);
    let mut text_font = font.clone();
    text_font.set_size(size);
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(rgb));
    match clip {
        Some((max_width, size)) => {
            canvas.save();
            canvas.clip_rect(
                Rect::from_xywh(x, baseline - size * 1.1, max_width, size * 1.5),
                skia_safe::ClipOp::Intersect,
                false,
            );
            canvas.draw_str(content, Point::new(x, baseline), &text_font, &paint);
            canvas.restore();
        }
        None => {
            canvas.draw_str(content, Point::new(x, baseline), &text_font, &paint);
        }
    }
}

/// 超宽时截断并加省略号;`max_width` 不够放省略号时返回空串。
fn ellipsize(font: &Font, size: f32, content: &str, max_width: f32) -> String {
    let mut text_font = font.clone();
    text_font.set_size(size);
    if text_font.measure_str(content, None).0 <= max_width {
        return content.to_string();
    }
    let ellipsis = "…";
    if text_font.measure_str(ellipsis, None).0 > max_width {
        return String::new();
    }
    let mut out = String::new();
    for ch in content.chars() {
        let mut next = out.clone();
        next.push(ch);
        next.push_str(ellipsis);
        if text_font.measure_str(&next, None).0 > max_width {
            break;
        }
        out.push(ch);
    }
    out.push_str(ellipsis);
    out
}

fn fill(rgb: u32) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(rgb));
    paint
}
