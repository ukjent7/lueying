//! 面板绘制:背景、预编辑条、候选行。
//!
//! 只认数据:令牌、尺寸与内容都由调用方给定,与 GPUI 路径共用同一套
//! 数值(server 传参),本文件不持主题常量。翻页箭头等交互件归窗口层。

use skia_safe::{Canvas, ClipOp, Color, Font, Paint, PaintStyle, Point, Rect, RRect, Vector};

/// 预编辑条高(逻辑像素);与 server 的 `PREEDIT_H` 一致。
pub const PREEDIT_H: f32 = 32.0;
/// 列表上内边距(`py_1`)。
const PAD_TOP: f32 = 4.0;
/// 候选间距(`gap_1`)。
pub const GAP: f32 = 4.0;
/// 列表左右内边距(`px_2`)。
const PAD_X: f32 = 8.0;
/// 底部内边距;窗口高度公式与 server 的 `PANEL_PAD_B` 一致。
const PAD_BOTTOM: f32 = 6.0;
/// 竖排列的左右内边距。
const COL_PAD_X: f32 = 6.0;
/// 面板与高亮块圆角。
const RADIUS: f32 = 8.0;
const ITEM_RADIUS: f32 = 4.0;

/// 面板外观令牌;字段与 server 的 `theme::Colors` 一一对应,值为 0xRRGGBB。
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: u32,
    pub surface_alt: u32,
    pub border: u32,
    pub text: u32,
    pub text_secondary: u32,
    pub text_tertiary: u32,
    pub accent: u32,
    pub accent_text: u32,
    pub selected_bg: u32,
    pub selected_text: u32,
}

/// 候选行字号与行高;随「候选字号」档位缩放,拼音行保持小字。
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    /// 候选文字字号。
    pub text: f32,
    /// 序号/注音字号。
    pub small: f32,
    /// 候选行高(含行距);窗口高度与菜单锚点用。
    pub row: f32,
}

impl Metrics {
    pub fn standard() -> Self {
        Self::scaled(1.0)
    }

    pub fn scaled(factor: f32) -> Self {
        Self {
            text: 17.0 * factor,
            small: 11.0 * factor,
            row: 34.0 * factor,
        }
    }

    /// 渲染行高;比窗口行高少 4px,补上列表的行间距合成整行。
    pub fn visual_row(&self) -> f32 {
        self.row - 4.0
    }

    /// 文字宽度折算成字号倍数:ASCII 约半个字宽,其余按全角算。
    /// 无字体可量时的估算路径,E2E 与无字体的光栅测试走这里。
    pub fn text_units(text: &str) -> f32 {
        text.chars()
            .map(|c| if c.is_ascii() { 0.53 } else { 1.0 })
            .sum()
    }
}

/// 全部绘制常量随 `metrics` 线性缩放(候选字号档 × 显示器 DPI);
/// 标准档 100% 缩放下 s=1,数值与 GPUI 路径的常量一致。
fn scale_of(metrics: &Metrics) -> f32 {
    metrics.row / 34.0
}

/// 预编辑条内容;页码标签只在多页时非空。
#[derive(Clone, Debug)]
pub struct PreeditBar {
    pub text: String,
    pub page_label: String,
}

/// 单条候选;`hint` 为注音/来源小字,空串不画。
#[derive(Clone, Debug)]
pub struct Item {
    pub text: String,
    pub hint: String,
    pub selected: bool,
}

/// 候选排列:横排挤一行自动换行,竖排一行一条。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Wrap,
    Column,
}

/// 一帧面板的全部输入。
#[derive(Clone, Debug)]
pub struct PanelSpec {
    pub width: f32,
    /// 候选文字的最大宽度(截断用)。
    pub max_text_width: f32,
    pub preedit: PreeditBar,
    pub items: Vec<Item>,
    pub layout: Layout,
}

fn color(rgb: u32) -> Color {
    Color::from_rgb(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

fn fill_paint(rgb: u32) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(rgb));
    paint
}

/// 单条候选的估算宽度:内边距、强调条、序号、文字与注音。
/// 有字体时实量,否则按字号倍率估算。
pub fn item_width(item: &Item, spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>) -> f32 {
    let s = scale_of(metrics);
    let text_width = match font {
        Some(font) => font.measure_str(&item.text, None).0,
        None => metrics.text * Metrics::text_units(&item.text),
    };
    let hint_width = if item.hint.is_empty() {
        0.0
    } else {
        let hint = match font {
            Some(font) => {
                let mut small = font.clone();
                small.set_size(metrics.small);
                small.measure_str(&item.hint, None).0
            }
            None => metrics.small * Metrics::text_units(&item.hint),
        }
        .min(100.0 * s);
        4.0 * s + hint
    };
    let hint_gap = if hint_width > 0.0 { 4.0 * s } else { 0.0 };
    // px_2(16)+强调条(2)+间距(4)+序号(12)+间距(4)+候选文字+注音。
    (16.0 + 2.0 + 4.0 + 12.0 + 4.0) * s + text_width + hint_gap + hint_width
}

/// 候选占几行。竖排一行一条;横排按实量/估算宽度换行。
pub fn layout_rows(spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>) -> usize {
    if matches!(spec.layout, Layout::Column) {
        return spec.items.len();
    }
    let available = panel_width(spec, metrics, font) - 18.0 * scale_of(metrics);
    let mut rows = 0usize;
    let mut used = 0.0f32;
    for item in &spec.items {
        let item = item_width(item, spec, metrics, font);
        if rows == 0 {
            rows = 1;
            used = item;
        } else if used + item + GAP > available {
            rows += 1;
            used = item;
        } else {
            used += item + GAP;
        }
    }
    rows
}

/// 窗口宽度:按全部候选估最宽一条,翻页时不来回跳。
pub fn panel_width(spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>) -> f32 {
    let s = scale_of(metrics);
    let widest = spec
        .items
        .iter()
        .map(|item| item_width(item, spec, metrics, font))
        .fold(0.0, f32::max);
    (widest + 18.0 * s).clamp(360.0 * s, 680.0 * s)
}

/// 窗口高度:预编辑条 + 当前行数 + 底边距;公式与 server 一致。
pub fn panel_height(spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>) -> f32 {
    let s = scale_of(metrics);
    PREEDIT_H * s + PAD_TOP * s + layout_rows(spec, metrics, font) as f32 * metrics.row + PAD_BOTTOM * s
}

/// 画一帧完整面板;返回实际画布占用(宽, 高)。
pub fn draw(
    canvas: &Canvas,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    font: Option<&Font>,
) -> (f32, f32) {
    let s = scale_of(metrics);
    let width = panel_width(spec, metrics, font);
    let height = panel_height(spec, metrics, font);

    // 背景圆角矩形 + 1px 描边(内缩 0.5px 让描边完整落在窗口里)。
    let body = Rect::from_xywh(0.5 * s, 0.5 * s, width - s, height - s);
    canvas.draw_rrect(
        RRect::new_rect_radii(body, &[Vector::new(RADIUS * s, RADIUS * s); 4]),
        &fill_paint(palette.bg),
    );
    let mut stroke = Paint::default();
    stroke.set_anti_alias(true);
    stroke.set_style(PaintStyle::Stroke);
    stroke.set_stroke_width(s);
    stroke.set_color(color(palette.border));
    canvas.draw_rrect(
        RRect::new_rect_radii(
            body,
            &[Vector::new((RADIUS - 0.5) * s, (RADIUS - 0.5) * s); 4],
        ),
        &stroke,
    );

    preedit_bar(canvas, width, spec, palette, metrics, font);
    items(canvas, width, spec, palette, metrics, font);
    (width, height)
}

fn preedit_bar(
    canvas: &Canvas,
    width: f32,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    font: Option<&Font>,
) {
    let s = scale_of(metrics);
    let mut paint = fill_paint(palette.surface_alt);
    paint.set_anti_alias(false);
    canvas.draw_rect(Rect::from_xywh(0.0, 0.0, width, PREEDIT_H * s), &paint);
    // 只有当前页有候选才画分隔线,空页不留一条孤线。
    if !spec.items.is_empty() {
        canvas.draw_rect(
            Rect::from_xywh(0.0, (PREEDIT_H - 1.0) * s, width, s),
            &fill_paint(palette.border),
        );
    }
    let Some(font) = font else { return };
    let mut font = font.clone();
    font.set_size(13.0 * s);
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(palette.text_secondary));
    let baseline = PREEDIT_H * s * 0.5 + 13.0 * s * 0.36;
    canvas.draw_str(
        &spec.preedit.text,
        Point::new(12.0 * s, baseline),
        &font,
        &paint,
    );
    if !spec.preedit.page_label.is_empty() {
        let (label_width, _) = font.measure_str(&spec.preedit.page_label, None);
        canvas.draw_str(
            &spec.preedit.page_label,
            Point::new(width - 12.0 * s - label_width, baseline),
            &font,
            &paint,
        );
    }
}

fn items(
    canvas: &Canvas,
    width: f32,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    font: Option<&Font>,
) {
    let s = scale_of(metrics);
    let row_h = metrics.visual_row();
    let wrap = matches!(spec.layout, Layout::Wrap);
    let (mut x, mut y) = if wrap {
        (PAD_X * s, (PREEDIT_H + PAD_TOP) * s)
    } else {
        (COL_PAD_X * s, (PREEDIT_H + PAD_TOP) * s)
    };
    let box_width = if wrap {
        0.0
    } else {
        width - COL_PAD_X * 2.0 * s
    };

    for (index, item) in spec.items.iter().enumerate() {
        let item_w = if wrap {
            item_width(item, spec, metrics, font)
        } else {
            box_width
        };
        if wrap && index > 0 && x + item_w > width - PAD_X * s {
            x = PAD_X * s;
            y += row_h + GAP * s;
        }
        // 选中块:柔和底色 + 左侧 2px 主色条。
        if item.selected {
            canvas.draw_rrect(
                RRect::new_rect_radii(
                    Rect::from_xywh(x, y, item_w, row_h),
                    &[Vector::new(ITEM_RADIUS * s, ITEM_RADIUS * s); 4],
                ),
                &fill_paint(palette.selected_bg),
            );
            canvas.draw_rrect(
                RRect::new_rect_radii(
                    Rect::from_xywh(
                        x + 8.0 * s,
                        y + (row_h - 14.0 * s) * 0.5,
                        2.0 * s,
                        14.0 * s,
                    ),
                    &[Vector::new(s, s); 4],
                ),
                &fill_paint(palette.accent_text),
            );
        }
        if let Some(font) = font {
            let mut small = font.clone();
            small.set_size(metrics.small);
            let index_color = if item.selected {
                palette.accent_text
            } else {
                palette.text_tertiary
            };
            let base = y + row_h * 0.5 + metrics.small * 0.36;
            // 序号
            text(
                canvas,
                &small,
                &format!("{}", index + 1),
                x + (8.0 + 2.0 + 4.0) * s,
                base,
                index_color,
                None,
            );
            // 候选文字
            let mut text_font = font.clone();
            text_font.set_size(metrics.text);
            let text_x = x + (8.0 + 2.0 + 4.0 + 12.0 + 4.0) * s;
            let text_color = if item.selected {
                palette.selected_text
            } else {
                palette.text
            };
            text(
                canvas,
                &text_font,
                &item.text,
                text_x,
                y + row_h * 0.5 + metrics.text * 0.36,
                text_color,
                Some((spec.max_text_width, metrics.text)),
            );
            // 注音/来源小字
            if !item.hint.is_empty() {
                let (text_width, _) = text_font.measure_str(&item.text, None);
                text(
                    canvas,
                    &small,
                    &item.hint,
                    text_x + text_width + 4.0 * s,
                    base,
                    index_color,
                    Some((100.0 * s, metrics.small)),
                );
            }
        }
        if wrap {
            x += item_w + GAP * s;
        } else {
            y += row_h + GAP * s;
        }
    }
}

/// 画一段文字;`clip` 给 (最大宽, 字号) 时先裁剪再画,超宽部分截掉。
fn text(
    canvas: &Canvas,
    font: &Font,
    content: &str,
    x: f32,
    baseline: f32,
    rgb: u32,
    clip: Option<(f32, f32)>,
) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(rgb));
    match clip {
        Some((max_width, size)) => {
            canvas.save();
            canvas.clip_rect(
                Rect::from_xywh(x, baseline - size * 1.1, max_width, size * 1.5),
                ClipOp::Intersect,
                false,
            );
            canvas.draw_str(content, Point::new(x, baseline), font, &paint);
            canvas.restore();
        }
        None => canvas.draw_str(content, Point::new(x, baseline), font, &paint),
    }
}
