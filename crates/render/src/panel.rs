//! 面板布局:窗口尺寸、行矩形与命中测试;绘制与命中共用同一份几何。
//!
//! 只认数据:令牌、尺寸与内容都由调用方给定(server 传参),本文件
//! 不持主题常量。文字宽度由 [`Measure`] 给出:Windows 侧用 DirectWrite
//! 实量,无字体的 E2E 与兜底路径用 [`Estimate`] 估算。布局与绘制必须
//! 用同一个实现,保证所指即所画。

use crate::paint::{Canvas, Color};

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

/// 布局矩形(逻辑像素);布局、命中与窗口层换算的通用单位。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub fn from_xywh(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }

    pub fn from_ltrb(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    pub fn center_x(&self) -> f32 {
        (self.left + self.right) / 2.0
    }

    pub fn center_y(&self) -> f32 {
        (self.top + self.bottom) / 2.0
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// 文字测宽;同一份布局必须自始至终用同一个实现,否则量出来的窗
/// 装不下画出来的字。
pub trait Measure {
    fn width(&self, text: &str, size: f32) -> f32;
}

/// 字号倍率估算:ASCII 约半宽,其余按全角。没有 DirectWrite 时的
/// 兜底(E2E 像素测试、光栅自检的估算路径)。
pub struct Estimate;

impl Measure for Estimate {
    fn width(&self, text: &str, size: f32) -> f32 {
        size * Metrics::text_units(text)
    }
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

/// 单条候选的估算宽度:内边距、强调条、序号、文字与注音。
pub fn item_width(item: &Item, metrics: &Metrics, measure: &dyn Measure) -> f32 {
    let s = scale_of(metrics);
    let text_width = measure.width(&item.text, metrics.text);
    let hint_width = if item.hint.is_empty() {
        0.0
    } else {
        4.0 * s + measure.width(&item.hint, metrics.small).min(100.0 * s)
    };
    let hint_gap = if hint_width > 0.0 { 4.0 * s } else { 0.0 };
    // px_2(16)+强调条(2)+间距(4)+序号(12)+间距(4)+候选文字+注音。
    (16.0 + 2.0 + 4.0 + 12.0 + 4.0) * s + text_width + hint_gap + hint_width
}

/// 候选占几行。竖排一行一条;横排按实量宽度换行。
pub fn layout_rows(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> usize {
    if matches!(spec.layout, Layout::Column) {
        return spec.items.len();
    }
    let available = panel_width(spec, metrics, measure) - 18.0 * scale_of(metrics);
    let mut rows = 0usize;
    let mut used = 0.0f32;
    for item in &spec.items {
        let item = item_width(item, metrics, measure);
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
pub fn panel_width(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> f32 {
    let s = scale_of(metrics);
    let widest = spec
        .items
        .iter()
        .map(|item| item_width(item, metrics, measure))
        .fold(0.0, f32::max);
    (widest + 18.0 * s)
        .max(spec.width)
        .clamp(360.0 * s, 680.0 * s)
}

/// 窗口高度:预编辑条 + 候选区 + 底边距;公式与 server 一致。
/// 末行后面没有行间距,所以行区按「行距 ×(行数−1) + 行高」算,
/// 高度公式:`PREEDIT_H + row × 行数 + PANEL_PAD_B`。
pub fn panel_height(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> f32 {
    let s = scale_of(metrics);
    let rows = layout_rows(spec, metrics, measure);
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
pub fn item_rects(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> Vec<Rect> {
    let s = scale_of(metrics);
    let width = panel_width(spec, metrics, measure);
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
            item_width(item, metrics, measure)
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
pub fn hit_test(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure, x: f32, y: f32) -> Hit {
    if !spec.preedit.page_label.is_empty() {
        let (prev, _, next) = pager_rects(panel_width(spec, metrics, measure), metrics);
        if prev.contains(x, y) {
            return Hit::PrevPage;
        }
        if next.contains(x, y) {
            return Hit::NextPage;
        }
    }
    for (index, rect) in item_rects(spec, metrics, measure).iter().enumerate() {
        if rect.contains(x, y) {
            return Hit::Item(index);
        }
    }
    Hit::Nothing
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

/// 面板底色由谁提供。
///
/// `Solid` 由本文件画实底、描边与投影;`Glass` 交给宿主用原生材质
/// (macOS 的液态玻璃)做背板,本文件只画内容,底色一律压成半透明,
/// 让材质透出来。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Surface {
    /// 实底:本文件画底色、描边与投影。
    #[default]
    Solid,
    /// 材质背板:宿主提供底色,本文件只画内容。
    Glass,
}

/// 一帧候选面板的绘制输入。
pub struct Frame<'a> {
    pub spec: &'a PanelSpec,
    pub palette: &'a Palette,
    pub metrics: Metrics,
    /// 底色由谁提供。
    pub surface: Surface,
    /// 页内悬停候选;`None` 表示没有。
    pub hover: Option<usize>,
    /// 悬停的翻页箭头。
    pub pager: Option<Hit>,
}

/// 画一帧候选面板:圆角底、拼音条、分页与候选行。
///
/// 这是跨平台画布的基准视觉,macOS 直接使用;Windows 的 D2D 路径另有
/// 动效与合成,不经过这里。布局与命中仍共用本文件的几何函数。
pub fn draw(canvas: &mut dyn Canvas, frame: &Frame<'_>, measure: &dyn Measure) {
    let s = frame.metrics.scale();
    let width = panel_width(frame.spec, &frame.metrics, measure);
    let height = panel_height(frame.spec, &frame.metrics, measure);
    let body = Rect::from_xywh(0.5 * s, 0.5 * s, width - s, height - s);
    let glass = frame.surface == Surface::Glass;
    // 材质背板上不透明色块会盖住材质,统一压成半透明。
    let tint = |rgb: u32, alpha: u8| {
        if glass {
            Color::with_alpha(rgb, alpha)
        } else {
            Color::opaque(rgb)
        }
    };
    if glass {
        // 材质自带边缘,这里只留一道极淡的描边把轮廓点出来。
        canvas.stroke_rrect(body, 8.0 * s, s, tint(frame.palette.border_strong, 0x4d));
    } else {
        canvas.shadow_rrect(
            Rect::from_xywh(2.0 * s, 3.0 * s, width - 4.0 * s, height - 6.0 * s),
            9.0 * s,
            16.0 * s,
            Color::with_alpha(0x000000, 56),
        );
        canvas.fill_rrect(body, 8.0 * s, Color::opaque(frame.palette.bg));
        canvas.stroke_rrect(
            Rect::from_xywh(
                body.left + 0.5 * s,
                body.top + 0.5 * s,
                (width - 2.0 * s).max(0.0),
                (height - 2.0 * s).max(0.0),
            ),
            7.5 * s,
            s,
            Color::opaque(frame.palette.border),
        );
    }

    // 拼音条:底色与一条只在有候选时出现的分隔线。顶部两角跟随面板圆角,
    // 下半段用矩形补齐,避免方块盖住外框。材质背板上不铺底色,只留分隔线。
    let bar = Rect::from_xywh(0.5 * s, 0.5 * s, width - s, (PREEDIT_H - 0.5) * s);
    if !glass {
        canvas.fill_rrect(
            Rect::from_xywh(bar.left, bar.top, bar.width(), 16.0 * s),
            7.5 * s,
            Color::opaque(frame.palette.surface_alt),
        );
        canvas.fill_rect(
            Rect::from_xywh(
                bar.left,
                bar.top + 8.0 * s,
                bar.width(),
                bar.height() - 8.0 * s,
            ),
            Color::opaque(frame.palette.surface_alt),
        );
    }
    if !frame.spec.items.is_empty() {
        // 实底用原来的分隔线颜色;材质背板上换用亮一档的描边色,压成半透明。
        let separator = if glass {
            Color::with_alpha(frame.palette.border_strong, 0x4d)
        } else {
            Color::opaque(frame.palette.border)
        };
        canvas.fill_rect(
            Rect::from_xywh(bar.left, (PREEDIT_H - 1.0) * s, bar.width(), s),
            separator,
        );
    }
    let has_pages = !frame.spec.preedit.page_label.is_empty();
    let (prev, label, next) = pager_rects(width, &frame.metrics);
    if has_pages {
        for (rect, hit) in [(prev, Hit::PrevPage), (next, Hit::NextPage)] {
            if frame.pager == Some(hit) {
                canvas.fill_rrect(rect, 6.0 * s, tint(frame.palette.border, 0x4d));
            }
        }
    }
    canvas.text(
        &frame.spec.preedit.text,
        crate::paint::Point::new(12.0 * s, PREEDIT_H * s * 0.5),
        13.0 * s,
        Color::opaque(frame.palette.text_secondary),
        Some((width - 100.0 * s).max(1.0)),
    );
    if has_pages {
        canvas.text_centered(
            "‹",
            prev,
            15.0 * s,
            Color::opaque(frame.palette.text_secondary),
        );
        canvas.text_centered(
            "›",
            next,
            15.0 * s,
            Color::opaque(frame.palette.text_secondary),
        );
        canvas.text_centered(
            &frame.spec.preedit.page_label,
            label,
            11.0 * s,
            Color::opaque(frame.palette.text_tertiary),
        );
    }

    let rects = item_rects(frame.spec, &frame.metrics, measure);
    for (index, (item, rect)) in frame.spec.items.iter().zip(&rects).enumerate() {
        let row_h = rect.height();
        if item.selected {
            canvas.fill_rrect(*rect, 4.0 * s, tint(frame.palette.selected_bg, 0xb3));
            canvas.fill_rrect(
                Rect::from_xywh(
                    rect.left + 8.0 * s,
                    rect.top + (row_h - 14.0 * s) * 0.5,
                    2.0 * s,
                    14.0 * s,
                ),
                s,
                Color::opaque(frame.palette.accent_text),
            );
        } else if frame.hover == Some(index) {
            canvas.fill_rrect(*rect, 4.0 * s, tint(frame.palette.hover, 0x66));
        }
        canvas.text(
            &format!("{}", index + 1),
            crate::paint::Point::new(rect.left + 14.0 * s, rect.center_y()),
            frame.metrics.small,
            Color::opaque(if item.selected {
                frame.palette.accent_text
            } else {
                frame.palette.text_tertiary
            }),
            Some(20.0 * s),
        );
        let text_x = rect.left + (8.0 + 2.0 + 4.0 + 12.0 + 4.0) * s;
        canvas.text(
            &item.text,
            crate::paint::Point::new(text_x, rect.center_y()),
            frame.metrics.text,
            Color::opaque(if item.selected {
                frame.palette.selected_text
            } else {
                frame.palette.text
            }),
            Some(frame.spec.max_text_width.max(1.0)),
        );
        if !item.hint.is_empty() {
            let text_width = measure.width(&item.text, frame.metrics.text);
            canvas.text(
                &item.hint,
                crate::paint::Point::new(text_x + text_width + 4.0 * s, rect.center_y()),
                frame.metrics.small,
                Color::opaque(frame.palette.text_tertiary),
                Some(100.0 * s),
            );
        }
    }
}
