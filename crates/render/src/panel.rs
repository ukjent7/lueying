//! 面板绘制:背景、预编辑条、候选行、右键菜单;命中测试与绘制共用布局。
//!
//! 只认数据:令牌、尺寸与内容都由调用方给定(server 传参),本文件
//! 不持主题常量。翻页箭头画在这里、鼠标命中
//! 判定也在这里,窗口层只把命中结果翻译成协议请求。

use skia_safe::{
    BlurStyle, Canvas, ClipOp, Color, Font, MaskFilter, Paint, PaintStyle, Point, RRect, Rect,
    Shader, Vector,
};

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
    /// 设置窗这类「整块窗口」的底色;候选窗本身不用它。
    pub window_bg: u32,
    pub surface_alt: u32,
    pub border: u32,
    /// 输入框、次级按钮的描边。
    pub border_strong: u32,
    pub text: u32,
    pub text_secondary: u32,
    pub text_tertiary: u32,
    pub accent: u32,
    /// 主色填充上的文字色。
    pub on_accent: u32,
    /// 分段控件选中格;浅色下浮起,深色下比轨道亮一档。
    pub segment: u32,
    /// 开关关闭时的轨道。
    pub off_bg: u32,
    /// 开关关闭时的旋钮。
    pub knob_off: u32,
    /// 危险操作(清空)的文字色与底色。
    pub danger: u32,
    pub danger_soft: u32,
    pub accent_text: u32,
    pub selected_bg: u32,
    pub selected_text: u32,
    /// 非选中候选的悬停底色。
    pub hover: u32,
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

    /// 绘制常量的缩放系数(候选字号档 × 显示器 DPI);菜单锚点等
    /// 窗口层换算也用它,保证命中与绘制一致。
    pub fn scale(&self) -> f32 {
        scale_of(self)
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
/// 标准档 100% 缩放下 s=1。
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
    /// 窗口宽度下限(已含缩放);0 表示只按候选估宽。
    pub width: f32,
    /// 候选文字的最大宽度(截断用)。
    pub max_text_width: f32,
    pub preedit: PreeditBar,
    pub items: Vec<Item>,
    pub layout: Layout,
}

/// 候选列表的动效:翻页滑入的横向位移与透明度;其余层不动。
#[derive(Clone, Copy, Debug)]
pub struct ListFx {
    pub dx: f32,
    pub opacity: f32,
}

impl ListFx {
    pub const NEUTRAL: ListFx = ListFx {
        dx: 0.0,
        opacity: 1.0,
    };
}

/// 鼠标命中区域;窗口层把命中翻译成点选/翻页请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// 背景或预编辑条空白处。
    Nothing,
    /// 页内第 index 条候选。
    Item(usize),
    /// 上一页箭头。
    PrevPage,
    /// 下一页箭头。
    NextPage,
}

/// 右键菜单项;动作与文案由宿主按同一套语义消费。
pub const MENU_ITEMS: [&str; 2] = ["置顶", "降权"];

pub(crate) fn color(rgb: u32) -> Color {
    Color::from_rgb(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

pub(crate) fn fill_paint(rgb: u32) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(rgb));
    paint
}

/// 候选正文与拼音的对比度下限(WCAG AA 正文)。
pub const CONTRAST_TEXT: f32 = 4.5;
/// 序号、注音、菜单项这些次要文字的对比度下限。
pub const CONTRAST_WEAK: f32 = 3.0;

/// 用户效果之上的遮罩不透明度。
///
/// 底色按这个不透明度压在用户效果上:效果最多只能改动底色的一小部分,
/// 文字对比度因此有下限。效果能画任意颜色,遮罩之后的面板底色落在一段
/// 区间里(最暗 = 底色本身,最亮 = 底色被纯白盖满);文字亮度若落在这段
/// 区间内,效果就能画出跟文字同色的底,对比度归零。这里按「离文字亮度
/// 最近的那一端」解出最小不透明度,再留一点余量。
pub fn scrim_alpha(base: u32, text: u32, target: f32) -> f32 {
    let base_c = channels(base);
    let text_l = luminance(channels(text));
    let mut alpha = 1.0f32;
    for step in 0..=64 {
        let weight = step as f32 / 64.0;
        let low = luminance(mix(base_c, [0.0, 0.0, 0.0], weight));
        let high = luminance(mix(base_c, [1.0, 1.0, 1.0], weight));
        let worst = if text_l <= low {
            contrast(low, text_l)
        } else if text_l >= high {
            contrast(high, text_l)
        } else {
            // 文字亮度落在可达区间里:效果能画出同色底,对比度归零。
            1.0
        };
        if worst >= target {
            alpha = weight;
            break;
        }
    }
    // 留一点余量,免得刚好卡在阈值上。
    (alpha + 0.04).min(0.95)
}

/// 面板底上的文字:候选正文与序号。
fn background_scrim(palette: &Palette) -> f32 {
    scrim_alpha(palette.bg, palette.text, CONTRAST_TEXT).max(scrim_alpha(
        palette.bg,
        palette.text_tertiary,
        CONTRAST_WEAK,
    ))
}

/// 选中块上的文字:候选正文与序号。
fn highlight_scrim(palette: &Palette) -> f32 {
    scrim_alpha(palette.selected_bg, palette.selected_text, CONTRAST_TEXT).max(scrim_alpha(
        palette.selected_bg,
        palette.accent_text,
        CONTRAST_WEAK,
    ))
}

/// 菜单上的文字:悬停项与普通项。
fn menu_scrim(palette: &Palette) -> f32 {
    scrim_alpha(palette.bg, palette.text, CONTRAST_TEXT).max(scrim_alpha(
        palette.bg,
        palette.text_secondary,
        CONTRAST_WEAK,
    ))
}

/// 遮罩:底色按不透明度压在用户效果上,文字画在遮罩之上。
fn scrim_paint(rgb: u32, alpha: f32) -> Paint {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(
        (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    ));
    paint
}

/// 0xRRGGBB 拆成 0..1 的 sRGB 三通道。
fn channels(rgb: u32) -> [f32; 3] {
    [
        ((rgb >> 16) & 0xff) as f32 / 255.0,
        ((rgb >> 8) & 0xff) as f32 / 255.0,
        (rgb & 0xff) as f32 / 255.0,
    ]
}

/// sRGB 通道值转线性值。
fn linearize(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// 相对亮度(WCAG 定义)。
fn luminance(rgb: [f32; 3]) -> f32 {
    0.2126 * linearize(rgb[0]) + 0.7152 * linearize(rgb[1]) + 0.0722 * linearize(rgb[2])
}

/// 两个亮度的对比度。
fn contrast(a: f32, b: f32) -> f32 {
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// 按权重混合两个颜色;Skia 在 sRGB 空间里就是这么混的。
fn mix(base: [f32; 3], other: [f32; 3], weight: f32) -> [f32; 3] {
    [
        base[0] * weight + other[0] * (1.0 - weight),
        base[1] * weight + other[1] * (1.0 - weight),
        base[2] * weight + other[2] * (1.0 - weight),
    ]
}

/// 一帧用到的效果:每个槽位一个 shader(窗口层按 uniform 生成)。
/// 槽位为空就画素色;效果层永远画在文字下面。
#[derive(Clone, Copy, Default)]
pub struct Effects<'a> {
    /// 面板底。
    pub background: Option<&'a Shader>,
    /// 选中候选的高亮块。
    pub highlight: Option<&'a Shader>,
    /// 右键菜单底。
    pub menu: Option<&'a Shader>,
}

/// 单条候选的估算宽度:内边距、强调条、序号、文字与注音。
/// 有字体时实量,否则按字号倍率估算。
pub fn item_width(item: &Item, metrics: &Metrics, font: Option<&Font>) -> f32 {
    let s = scale_of(metrics);
    let text_width = match font {
        // 量宽必须与绘制同字号,否则 DPI/字号档放大后窗口会窄一截。
        Some(font) => {
            let mut text_font = font.clone();
            text_font.set_size(metrics.text);
            text_font.measure_str(&item.text, None).0
        }
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
        let item = item_width(item, metrics, font);
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
/// `spec.width` 是全组合的宽度下限(已含缩放),0 表示只按本页候选估。
pub fn panel_width(spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>) -> f32 {
    let s = scale_of(metrics);
    let widest = spec
        .items
        .iter()
        .map(|item| item_width(item, metrics, font))
        .fold(0.0, f32::max);
    (widest + 18.0 * s)
        .max(spec.width)
        .clamp(360.0 * s, 680.0 * s)
}

/// 窗口高度:预编辑条 + 候选区 + 底边距;公式与 server 一致。
/// 末行后面没有行间距,所以行区按「行距 ×(行数−1) + 行高」算,
/// 高度公式:`PREEDIT_H + row × 行数 + PANEL_PAD_B`。
pub fn panel_height(spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>) -> f32 {
    let s = scale_of(metrics);
    let rows = layout_rows(spec, metrics, font);
    if rows == 0 {
        return PREEDIT_H * s + PAD_BOTTOM * s;
    }
    PREEDIT_H * s
        + PAD_TOP * s
        + (rows as f32 - 1.0) * metrics.row
        + metrics.visual_row()
        + PAD_BOTTOM * s
}

/// 候选行的布局矩形(画布坐标);绘制与命中测试共用,所指即所画。
pub fn item_rects(spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>) -> Vec<Rect> {
    let s = scale_of(metrics);
    let width = panel_width(spec, metrics, font);
    let row_h = metrics.visual_row();
    let wrap = matches!(spec.layout, Layout::Wrap);
    let (start_x, mut y) = if wrap {
        (PAD_X * s, (PREEDIT_H + PAD_TOP) * s)
    } else {
        (COL_PAD_X * s, (PREEDIT_H + PAD_TOP) * s)
    };
    let mut x = start_x;
    let box_width = if wrap {
        0.0
    } else {
        width - COL_PAD_X * 2.0 * s
    };
    let mut rects = Vec::with_capacity(spec.items.len());
    for item in &spec.items {
        let item_w = if wrap {
            item_width(item, metrics, font)
        } else {
            box_width
        };
        if wrap && !rects.is_empty() && x + item_w > width - PAD_X * s {
            x = start_x;
            y += row_h + GAP * s;
        }
        rects.push(Rect::from_xywh(x, y, item_w, row_h));
        if wrap {
            x += item_w + GAP * s;
        } else {
            y += row_h + GAP * s;
        }
    }
    rects
}

/// 预编辑条右侧的「‹ 页码 ›」布局:箭头 22×22,页码区 28 宽,间距 4。
/// 返回 (上一页箭头, 页码区, 下一页箭头)。
pub fn pager_rects(width: f32, metrics: &Metrics) -> (Rect, Rect, Rect) {
    let s = scale_of(metrics);
    let ah = 22.0 * s;
    let ay = (PREEDIT_H * s - ah) * 0.5;
    let next = Rect::from_xywh(width - 34.0 * s, ay, ah, ah);
    let label = Rect::from_xywh(width - 66.0 * s, ay, 28.0 * s, ah);
    let prev = Rect::from_xywh(width - 92.0 * s, ay, ah, ah);
    (prev, label, next)
}

/// 鼠标位置(画布坐标)→ 命中区域;箭头只在多页时存在。
pub fn hit_test(spec: &PanelSpec, metrics: &Metrics, font: Option<&Font>, x: f32, y: f32) -> Hit {
    if !spec.preedit.page_label.is_empty() {
        let (prev, _, next) = pager_rects(panel_width(spec, metrics, font), metrics);
        if point_in(prev, x, y) {
            return Hit::PrevPage;
        }
        if point_in(next, x, y) {
            return Hit::NextPage;
        }
    }
    for (index, rect) in item_rects(spec, metrics, font).iter().enumerate() {
        if point_in(*rect, x, y) {
            return Hit::Item(index);
        }
    }
    Hit::Nothing
}

fn point_in(rect: Rect, x: f32, y: f32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

/// 画一帧完整面板(静态路径:E2E 与自检用);返回实际画布占用(宽, 高)。
pub fn draw(
    canvas: &Canvas,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    font: Option<&Font>,
) -> (f32, f32) {
    let width = panel_width(spec, metrics, font);
    let height = panel_height(spec, metrics, font);
    draw_chrome(
        canvas,
        width,
        height,
        spec,
        palette,
        metrics,
        font,
        None,
        Effects::default(),
    );
    draw_items(
        canvas,
        width,
        height,
        spec,
        palette,
        metrics,
        font,
        ListFx::NEUTRAL,
        None,
        Effects::default(),
    );
    (width, height)
}

/// 背景圆角矩形、描边与预编辑条(含翻页箭头);候选列表之外的全部静态层。
#[allow(clippy::too_many_arguments)]
pub fn draw_chrome(
    canvas: &Canvas,
    width: f32,
    height: f32,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    font: Option<&Font>,
    pager: Option<Hit>,
    effects: Effects,
) {
    let s = scale_of(metrics);
    // 背景圆角矩形 + 1px 描边(内缩 0.5px 让描边完整落在窗口里)。
    let body = Rect::from_xywh(0.5 * s, 0.5 * s, width - s, height - s);
    let rounded = RRect::new_rect_radii(body, &[Vector::new(RADIUS * s, RADIUS * s); 4]);
    // 底色始终不透明:效果叠在底色上,文字画在最上面。
    canvas.draw_rrect(rounded, &fill_paint(palette.bg));
    // 面板底效果盖住底色;遮罩再压一层,文字对比度因此有下限。
    if let Some(shader) = effects.background {
        let mut effect = fill_paint(palette.bg);
        effect.set_shader(shader.clone());
        canvas.draw_rrect(rounded, &effect);
        canvas.draw_rrect(rounded, &scrim_paint(palette.bg, background_scrim(palette)));
    }
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
    // 拼音条满宽;按面板圆角裁掉顶角(等价于 overflow_hidden)。
    canvas.save();
    canvas.clip_rrect(rounded, ClipOp::Intersect, true);
    preedit_bar(canvas, width, spec, palette, metrics, font, pager);
    canvas.restore();
}

/// 候选列表;`fx` 带翻页滑动位移与列表透明度,`hover` 是页内悬停下标。
#[allow(clippy::too_many_arguments)]
pub fn draw_items(
    canvas: &Canvas,
    width: f32,
    height: f32,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    font: Option<&Font>,
    fx: ListFx,
    hover: Option<usize>,
    effects: Effects,
) {
    let s = scale_of(metrics);
    canvas.save();
    // 翻页滑动会把候选推出面板;裁到描边内侧(等价于 overflow_hidden)。
    canvas.clip_rect(
        Rect::from_xywh(
            s,
            s,
            (width - 2.0 * s).max(0.0),
            (height - 2.0 * s).max(0.0),
        ),
        ClipOp::Intersect,
        false,
    );
    // 滑动/淡入的透明度只作用于列表层,背景与拼音条保持稳定。
    if fx.opacity < 1.0 {
        // 图层范围按未平移的列表区算,平移只影响内容。
        let list = Rect::from_xywh(0.0, PREEDIT_H * s, width, (height - PREEDIT_H * s).max(0.0));
        canvas.save_layer_alpha_f(list, fx.opacity.clamp(0.0, 1.0));
    }
    if fx.dx != 0.0 {
        canvas.translate((fx.dx, 0.0));
    }
    let rects = item_rects(spec, metrics, font);
    for (index, (item, rect)) in spec.items.iter().zip(&rects).enumerate() {
        let (x, y) = (rect.left, rect.top);
        let row_h = rect.height();
        // 选中块:柔和底色 + 左侧 2px 主色条;悬停块只给非选中行。
        if item.selected {
            let selected_rect =
                RRect::new_rect_radii(*rect, &[Vector::new(ITEM_RADIUS * s, ITEM_RADIUS * s); 4]);
            canvas.draw_rrect(selected_rect, &fill_paint(palette.selected_bg));
            // 选中块效果盖住底色;遮罩压住它,选中文字照样读得清。
            if let Some(shader) = effects.highlight {
                let mut effect = fill_paint(palette.selected_bg);
                effect.set_shader(shader.clone());
                canvas.draw_rrect(selected_rect, &effect);
                canvas.draw_rrect(
                    selected_rect,
                    &scrim_paint(palette.selected_bg, highlight_scrim(palette)),
                );
            }
            canvas.draw_rrect(
                RRect::new_rect_radii(
                    Rect::from_xywh(x + 8.0 * s, y + (row_h - 14.0 * s) * 0.5, 2.0 * s, 14.0 * s),
                    &[Vector::new(s, s); 4],
                ),
                &fill_paint(palette.accent_text),
            );
        } else if hover == Some(index) {
            canvas.draw_rrect(
                RRect::new_rect_radii(*rect, &[Vector::new(ITEM_RADIUS * s, ITEM_RADIUS * s); 4]),
                &fill_paint(palette.hover),
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
    }
    if fx.opacity < 1.0 {
        canvas.restore();
    }
    canvas.restore();
}

/// 画右键菜单;`(x, y)` 是菜单左上角,`hover` 是悬停的菜单项,尺寸随缩放。
#[allow(clippy::too_many_arguments)]
pub fn draw_menu(
    canvas: &Canvas,
    x: f32,
    y: f32,
    palette: &Palette,
    metrics: &Metrics,
    font: Option<&Font>,
    hover: Option<usize>,
    effects: Effects,
) {
    let s = scale_of(metrics);
    let w = 116.0 * s;
    let h = menu_height(metrics);
    // 轻投影把菜单从候选层上托起来。
    let mut shadow = Paint::default();
    shadow.set_anti_alias(true);
    shadow.set_color(Color::from_argb(28, 0, 0, 0));
    shadow.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, 6.0 * s, None));
    canvas.draw_rrect(
        RRect::new_rect_radii(
            Rect::from_xywh(x + s, y + 2.0 * s, w, h),
            &[Vector::new(4.0 * s, 4.0 * s); 4],
        ),
        &shadow,
    );
    let body = Rect::from_xywh(x + 0.5 * s, y + 0.5 * s, w - s, h - s);
    let menu_rect = RRect::new_rect_radii(body, &[Vector::new(4.0 * s, 4.0 * s); 4]);
    canvas.draw_rrect(menu_rect, &fill_paint(palette.bg));
    // 菜单底效果盖住底色;遮罩压住它,菜单文字照样读得清。
    if let Some(shader) = effects.menu {
        let mut effect = fill_paint(palette.bg);
        effect.set_shader(shader.clone());
        canvas.draw_rrect(menu_rect, &effect);
        canvas.draw_rrect(menu_rect, &scrim_paint(palette.bg, menu_scrim(palette)));
    }
    let mut stroke = Paint::default();
    stroke.set_anti_alias(true);
    stroke.set_style(PaintStyle::Stroke);
    stroke.set_stroke_width(s);
    stroke.set_color(color(palette.border));
    canvas.draw_rrect(
        RRect::new_rect_radii(body, &[Vector::new(3.5 * s, 3.5 * s); 4]),
        &stroke,
    );
    // 悬停行底色先画:没有字体时也要留住反馈。
    if let Some(index) = hover {
        let row_y = y + (4.0 + index as f32 * 28.0) * s;
        canvas.draw_rrect(
            RRect::new_rect_radii(
                Rect::from_xywh(x + 4.0 * s, row_y, (w - 8.0 * s).max(0.0), 26.0 * s),
                &[Vector::new(4.0 * s, 4.0 * s); 4],
            ),
            &fill_paint(palette.hover),
        );
    }
    let Some(font) = font else { return };
    let mut item_font = font.clone();
    item_font.set_size(12.0 * s);
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    for (index, label) in MENU_ITEMS.iter().enumerate() {
        let row_y = y + (4.0 + index as f32 * 28.0) * s;
        paint.set_color(color(if hover == Some(index) {
            palette.text
        } else {
            palette.text_secondary
        }));
        let baseline = row_y + 26.0 * s * 0.5 + 12.0 * s * 0.36;
        canvas.draw_str(
            *label,
            Point::new(x + 8.0 * s, baseline),
            &item_font,
            &paint,
        );
    }
}

/// 菜单高度:上下内边距 4 + 两行 26 + 行间距 2,加边框。
pub fn menu_height(metrics: &Metrics) -> f32 {
    64.0 * scale_of(metrics)
}

/// 菜单命中:返回菜单项下标(0=置顶, 1=降权);菜单外与行间空隙返回 None。
pub fn menu_hit(x: f32, y: f32, metrics: &Metrics, mx: f32, my: f32) -> Option<usize> {
    let s = scale_of(metrics);
    if mx < x || mx >= x + 116.0 * s || my < y || my >= y + menu_height(metrics) {
        return None;
    }
    let rel = my - y - 4.0 * s;
    if rel < 26.0 * s {
        Some(0)
    } else if rel >= 28.0 * s && rel < 54.0 * s {
        Some(1)
    } else {
        None
    }
}

fn preedit_bar(
    canvas: &Canvas,
    width: f32,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    font: Option<&Font>,
    pager: Option<Hit>,
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
    // 翻页箭头:悬停底色是几何,没有字体也要画(无字体路径也看得出悬停反馈);
    // 箭头字形与页码要字体,留到后面。
    let has_pages = !spec.preedit.page_label.is_empty();
    let (prev, label, next) = pager_rects(width, metrics);
    if has_pages {
        for (rect, hit) in [(prev, Hit::PrevPage), (next, Hit::NextPage)] {
            if pager == Some(hit) {
                // 悬停反馈:一层圆角描边色底。
                canvas.draw_rrect(
                    RRect::new_rect_radii(rect, &[Vector::new(6.0 * s, 6.0 * s); 4]),
                    &fill_paint(palette.border),
                );
            }
        }
    }
    let Some(font) = font else { return };
    // 拼音行:13px,左侧 12 内边距。
    let mut preedit_font = font.clone();
    preedit_font.set_size(13.0 * s);
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(color(palette.text_secondary));
    let baseline = PREEDIT_H * s * 0.5 + 13.0 * s * 0.36;
    canvas.draw_str(
        &spec.preedit.text,
        Point::new(12.0 * s, baseline),
        &preedit_font,
        &paint,
    );
    // 页码与翻页箭头;单页时都没有。
    if !has_pages {
        return;
    }
    let mut arrow_font = font.clone();
    arrow_font.set_size(15.0 * s);
    for (glyph, rect) in [("‹", prev), ("›", next)] {
        let (glyph_width, _) = arrow_font.measure_str(glyph, None);
        let mid = (rect.top + rect.bottom) * 0.5 + 15.0 * s * 0.36;
        canvas.draw_str(
            glyph,
            Point::new(rect.left + (rect.width() - glyph_width) * 0.5, mid),
            &arrow_font,
            &paint,
        );
    }
    let mut label_font = font.clone();
    label_font.set_size(11.0 * s);
    paint.set_color(color(palette.text_tertiary));
    let (label_width, _) = label_font.measure_str(&spec.preedit.page_label, None);
    let mid = (label.top + label.bottom) * 0.5 + 11.0 * s * 0.36;
    canvas.draw_str(
        &spec.preedit.page_label,
        Point::new(label.left + (label.width() - label_width) * 0.5, mid),
        &label_font,
        &paint,
    );
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
        None => {
            canvas.draw_str(content, Point::new(x, baseline), font, &paint);
        }
    }
}
