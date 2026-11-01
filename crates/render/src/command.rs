//! 命令面板绘制:输入框、竖排命令列表、页脚与命中。
//!
//! 只认数据:查询串、可见行与选中下标由服务端的命令搜索模型给出,调色板
//! 由窗口层传入。绘制与命中共用同一份 [`Layout`],所指即所画。

use crate::paint::{Canvas, Color, Point};
use crate::panel::{Palette, Rect};

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
pub fn draw(canvas: &mut dyn Canvas, spec: &Spec, layout: &Layout, palette: &Palette, scale: f32) {
    let scale = scale.max(0.5);
    let (width, height) = layout.size;
    canvas.fill_rect(
        Rect::from_xywh(0.0, 0.0, width, height),
        Color::opaque(palette.bg),
    );
    // 输入框与列表之间的分隔线。
    canvas.fill_rect(
        Rect::from_xywh(0.0, layout.input.bottom - scale, width, scale),
        Color::opaque(palette.border),
    );
    // 外框;窗口没有系统边框,描边由这里画。
    canvas.stroke_rect(
        Rect::from_xywh(0.5 * scale, 0.5 * scale, width - scale, height - scale),
        scale,
        Color::opaque(palette.border),
    );

    draw_input(canvas, spec, layout, palette, scale);
    draw_rows(canvas, spec, layout, palette, scale);
    draw_footer(canvas, spec, layout, palette, scale);
}

fn draw_input(
    canvas: &mut dyn Canvas,
    spec: &Spec,
    layout: &Layout,
    palette: &Palette,
    scale: f32,
) {
    let size = 15.0 * scale;
    let middle = layout.input.center_y();
    let x = PAD * scale;
    let max_width = layout.input.width() - 2.0 * PAD * scale;
    if spec.query.is_empty() {
        text(
            canvas,
            size,
            palette.text_tertiary,
            Point::new(x, middle),
            "输入拼音搜索命令",
            Some(max_width),
        );
        // 空查询时把光标画在提示文字前。
        caret(canvas, x, layout.input, palette, scale);
        return;
    }
    let shown = ellipsize(canvas, size, &spec.query, max_width - 2.0 * scale);
    let query_width = canvas.measure(&shown, size);
    text(
        canvas,
        size,
        palette.text,
        Point::new(x, middle),
        &shown,
        Some(max_width),
    );
    caret(
        canvas,
        x + query_width + 2.0 * scale,
        layout.input,
        palette,
        scale,
    );
}

fn caret(canvas: &mut dyn Canvas, x: f32, input: Rect, palette: &Palette, scale: f32) {
    let x = x.min(input.right - PAD * scale);
    let height = 18.0 * scale;
    canvas.fill_rect(
        Rect::from_xywh(x, input.center_y() - height * 0.5, 1.5 * scale, height),
        Color::opaque(palette.accent),
    );
}

fn draw_rows(canvas: &mut dyn Canvas, spec: &Spec, layout: &Layout, palette: &Palette, scale: f32) {
    if spec.rows.is_empty() {
        let message = if spec.total == 0 && spec.query.is_empty() {
            "没有可用的命令"
        } else {
            "没有匹配的命令"
        };
        let size = 14.0 * scale;
        let width = canvas.measure(message, size);
        text(
            canvas,
            size,
            palette.text_secondary,
            Point::new(layout.list.center_x() - width * 0.5, layout.list.center_y()),
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
            canvas.fill_rrect(inner, RADIUS * scale, Color::opaque(palette.selected_bg));
        }
        let extension_width = canvas.measure(&row.extension, small);
        let extension_x = rect.right - PAD * scale - extension_width;
        let title_width = (extension_x - rect.left - PAD * scale - 10.0 * scale).max(0.0);
        let title = ellipsize(canvas, size, &row.title, title_width);
        let middle = rect.center_y();
        text(
            canvas,
            size,
            if selected {
                palette.selected_text
            } else {
                palette.text
            },
            Point::new(rect.left + PAD * scale, middle),
            &title,
            Some(title_width),
        );
        text(
            canvas,
            small,
            if selected {
                palette.accent_text
            } else {
                palette.text_tertiary
            },
            Point::new(extension_x, middle),
            &row.extension,
            None,
        );
    }
    // 卷动提示:上下还有内容时在列表右缘画小箭头。
    if spec.above > 0 {
        text(
            canvas,
            10.0 * scale,
            palette.text_tertiary,
            Point::new(
                layout.list.right - 10.0 * scale,
                layout.list.top + 5.0 * scale,
            ),
            "▲",
            None,
        );
    }
    if spec.below > 0 {
        text(
            canvas,
            10.0 * scale,
            palette.text_tertiary,
            Point::new(
                layout.list.right - 10.0 * scale,
                layout.list.bottom - 5.0 * scale,
            ),
            "▼",
            None,
        );
    }
}

fn draw_footer(
    canvas: &mut dyn Canvas,
    spec: &Spec,
    layout: &Layout,
    palette: &Palette,
    scale: f32,
) {
    let size = 11.0 * scale;
    let middle = layout.footer.center_y();
    text(
        canvas,
        size,
        palette.text_tertiary,
        Point::new(PAD * scale, middle),
        "↑↓ 选择 · Enter 执行 · Esc 关闭",
        Some(layout.footer.width() - 2.0 * PAD * scale),
    );
    if spec.total == 0 {
        return;
    }
    let label = format!("{}/{}", spec.above + spec.selected + 1, spec.total);
    let label_width = canvas.measure(&label, size);
    text(
        canvas,
        size,
        palette.text_tertiary,
        Point::new(layout.footer.right - PAD * scale - label_width, middle),
        &label,
        None,
    );
}

/// 鼠标命中;坐标是窗口客户区物理像素。
pub fn hit(layout: &Layout, spec: &Spec, x: f32, y: f32) -> Hit {
    if layout.input.contains(x, y) {
        return Hit::Input;
    }
    for (index, rect) in layout.rows.iter().enumerate() {
        if rect.contains(x, y) && index < spec.rows.len() {
            return Hit::Row(index);
        }
    }
    if layout.footer.contains(x, y) {
        return Hit::Footer;
    }
    Hit::Nothing
}

/// 画一段文字;`max_width` 给定时裁掉超出部分。
pub(crate) fn text(
    canvas: &mut dyn Canvas,
    size: f32,
    rgb: u32,
    origin: Point,
    content: &str,
    max_width: Option<f32>,
) {
    canvas.text(content, origin, size, Color::opaque(rgb), max_width);
}

/// 超宽时截断并加省略号;`max_width` 不够放省略号时返回空串。
pub(crate) fn ellipsize(
    canvas: &mut dyn Canvas,
    size: f32,
    content: &str,
    max_width: f32,
) -> String {
    if canvas.measure(content, size) <= max_width {
        return content.to_string();
    }
    let ellipsis = "…";
    if canvas.measure(ellipsis, size) > max_width {
        return String::new();
    }
    let mut out = String::new();
    for ch in content.chars() {
        let mut next = out.clone();
        next.push(ch);
        next.push_str(ellipsis);
        if canvas.measure(&next, size) > max_width {
            break;
        }
        out.push(ch);
    }
    out.push_str(ellipsis);
    out
}
