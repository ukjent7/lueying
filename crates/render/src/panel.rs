//! 面板布局:窗口尺寸、行矩形与命中测试;绘制与命中共用同一份几何。
//!
//! 只认数据:令牌、尺寸与内容都由调用方给定(server 传参),本文件
//! 不持主题常量。文字宽度由 [`Measure`] 给出:Windows 侧用 DirectWrite
//! 实量,无字体的 E2E 与兜底路径用 [`Estimate`] 估算。布局与绘制必须
//! 用同一个实现,保证所指即所画。
//!
//! 候选窗有两个形态:组句中的**固定条**(宽度恒定、单行等宽槽、超宽
//! 截断,对齐 macOS 候选条)与**展开棋盘**(Tab 或条尾 ⌄ 切换,等宽等高
//! 的 4×6 单元,收起按钮浮在首行右端)。只有展开才形变。
//!
//! 圆角按 Apple 的三类形状:固定、胶囊、同心。候选窗是胶囊(半径 =
//! 半高);槽内胶囊半径 = 窗半径 − 上下留白,左右留白与上下相等,
//! 两级圆角共用同一个圆心。菜单、徽标、工具栏是固定半径的控件。

use crate::paint::{Canvas, Color, Point};

/// 列表上下内边距;上下对称,行区在窗口里竖直居中。
const PAD_Y: f32 = 5.0;
/// 候选间距(`gap_1`)。
pub const GAP: f32 = 4.0;
/// 列表左右内边距;与上下留白相等,内层胶囊才能和窗口圆角同心。
pub const PAD_X: f32 = PAD_Y;
/// 候选窗圆角(逻辑像素);标准行高 40 时正好是胶囊(半高)。
pub const PANEL_RADIUS: f32 = 20.0;
/// 槽内左右留白;选中槽是胶囊,两端有弧,文字与序号都要收进来。
pub const ITEM_PAD: f32 = 10.0;
/// 序号字宽。
const INDEX_W: f32 = 12.0;
/// 序号与候选文字的间距。
const INDEX_GAP: f32 = 4.0;
/// 候选文字距槽左缘的距离;布局、绘制与命中共用。
pub const TEXT_X: f32 = ITEM_PAD + INDEX_W + INDEX_GAP;
/// 竖排列沿用同一圈留白,整宽槽的圆角才能和窗口同心。
const COL_PAD_X: f32 = PAD_X;

/// 固定条的窗口宽度(逻辑像素);打字期间恒定,内容超宽截断。
pub const STRIP_W: f32 = 480.0;
/// 条尾展开按钮(⌄)的方格边长。
const CHEVRON: f32 = 22.0;
/// 条尾按钮距右缘;圆角半径差 = 20 − 11,按钮圆心与窗口端帽圆心重合。
const CHEVRON_INSET: f32 = PANEL_RADIUS - CHEVRON * 0.5;
/// 展开棋盘的列数;480 逻辑宽下每格约 114×30,四字词也装得下。
pub const GRID_COLS: usize = 4;
/// 展开棋盘的行数;滚动按整行推进,候选列始终对齐。
pub const GRID_ROWS: usize = 6;
/// 展开棋盘一屏多少条候选:4×6 满格。收起钮浮在首行右端的候选格角上,
/// 不占候选格,四列每行都是满的,滚动时列不会错位。
pub const GRID_MAX: usize = GRID_COLS * GRID_ROWS;
/// 滚动条静止/悬停宽度:细条不挡候选,悬停时展开到方便抓取。
pub const SCROLLBAR_IDLE: f32 = 3.0;
pub const SCROLLBAR_HOVER: f32 = 7.0;
/// 右侧控制轨与棋盘之间的间距;收起钮和滚动条共用这一条竖线。
const RAIL_GAP: f32 = 2.0;
/// 控制轨距窗口右缘的留白,与候选列表的上下留白一致。
const RAIL_INSET: f32 = PAD_X;

/// 候选排列:横排挤一行自动换行,竖排一行一条。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Wrap,
    Column,
}

/// 展开网格的一条候选;`index` 是全量候选里的绝对序号,点选走 `PickIndex`。
#[derive(Clone, Debug)]
pub struct GridItem {
    pub text: String,
    pub index: usize,
    /// 当前高亮;棋盘里垫整格黄釉胶囊。
    pub selected: bool,
}

/// 展开棋盘的滚动条:一屏放 `cap` 条,当前从 `offset` 开始。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scroll {
    pub offset: usize,
    pub len: usize,
    pub cap: usize,
}

/// 一帧面板的全部输入。
#[derive(Clone, Debug)]
pub struct PanelSpec {
    /// 展开态的估宽下限(已含缩放);固定条忽略它。
    pub width: f32,
    /// 候选文字的最大宽度(截断用)。
    pub max_text_width: f32,
    /// true = 展开网格;false = 固定条。竖排一律按自然尺寸,忽略此位。
    pub expand: bool,
    /// 当前页候选。
    pub items: Vec<Item>,
    /// 展开棋盘的候选:当前可见的一屏,点选带绝对序号。
    /// 滚动时由 `scroll` 指明这一屏在完整候选列表里的起点。
    pub grid: Vec<GridItem>,
    /// 展开棋盘的滚动位置;候选不足一屏或固定条为 `None`。
    pub scroll: Option<Scroll>,
    pub layout: Layout,
}

impl PanelSpec {
    /// 固定条形态只属于横排;竖排与展开互斥。
    pub fn is_strip(&self) -> bool {
        !self.expand && matches!(self.layout, Layout::Wrap)
    }
}

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

impl Palette {
    /// 浅色令牌;Linux 与 macOS 候选窗直接使用。
    pub const LIGHT: Palette = Palette {
        bg: 0xffffff,
        window_bg: 0xf3f4f7,
        surface_alt: 0xf7f8fa,
        border: 0xe2e4e9,
        border_strong: 0xcfd2d9,
        text: 0x1b1c1f,
        text_secondary: 0x5f636b,
        text_tertiary: 0x6a6e77,
        accent: 0x2f6fed,
        on_accent: 0xffffff,
        segment: 0xffffff,
        off_bg: 0xd9dce3,
        knob_off: 0xffffff,
        danger: 0xb3261e,
        danger_soft: 0xfdecea,
        accent_text: 0x2459c9,
        selected_bg: 0xeaf1ff,
        selected_text: 0x1b1c1f,
        hover: 0xf0f2f6,
    };

    /// 深色令牌,与 `LIGHT` 配套。
    pub const DARK: Palette = Palette {
        bg: 0x202127,
        window_bg: 0x17181c,
        surface_alt: 0x262830,
        border: 0x30323a,
        border_strong: 0x42454f,
        text: 0xecedf2,
        text_secondary: 0xa6aab4,
        text_tertiary: 0x8e929c,
        accent: 0x2f6fed,
        on_accent: 0xffffff,
        segment: 0x343741,
        off_bg: 0x3a3d46,
        knob_off: 0xffffff,
        danger: 0xf28b82,
        danger_soft: 0x3a2424,
        accent_text: 0x7aa7ff,
        selected_bg: 0x2a3b57,
        selected_text: 0xecedf2,
        hover: 0x2a2c34,
    };
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

/// 同心圆角:内层半径 = 外层半径 − 间距;间距过大时退回 `minimum`。
/// Apple 的 `ConcentricRectangle` 用同一定义,胶囊是半径等于半高的特例。
pub fn concentric_radius(outer: f32, gap: f32, minimum: f32) -> f32 {
    (outer - gap).max(minimum)
}

/// 选中槽釉面的上下两端:玻璃把亮面黄的明度拉开,底缘往琥珀收,
/// 实底主题只轻抬一档,系统蓝保留原生身份。
pub fn selection_stops(rgb: u32, glass: bool) -> (u32, u32) {
    let scale = |factor: f32| {
        let channel =
            |shift: u32| (((rgb >> shift) & 0xff) as f32 * factor).clamp(0.0, 255.0) as u32;
        (channel(16) << 16) | (channel(8) << 8) | channel(0)
    };
    if glass {
        // 黄釉底缘偏琥珀:绿色压得比红色多,蓝色收到最低,免得发橄榄。
        let channel = |shift: u32, factor: f32| {
            (((rgb >> shift) & 0xff) as f32 * factor).clamp(0.0, 255.0) as u32
        };
        let bottom = (channel(16, 0.88) << 16) | (channel(8, 0.79) << 8) | channel(0, 0.60);
        (scale(1.12), bottom)
    } else {
        (scale(1.06), scale(0.94))
    }
}

/// 胶囊在 `y` 处的水平内缩;贴边的高光与釉根线收进圆角里,不会越出轮廓。
pub fn capsule_inset(rect: Rect, y: f32) -> f32 {
    let radius = rect.height() * 0.5;
    let dy = (y - rect.center_y()).abs();
    radius - (radius * radius - dy * dy).max(0.0).sqrt()
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

/// 单条候选;`hint` 为注音/来源小字,空串不画。
#[derive(Clone, Debug)]
pub struct Item {
    pub text: String,
    pub hint: String,
    pub selected: bool,
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

/// 鼠标命中区域;窗口层把命中翻译成点选/展开请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// 背景或空白处。
    Nothing,
    /// 当前页第 index 条候选。
    Item(usize),
    /// 展开网格第 index(绝对序号)条候选。
    GridItem(usize),
    /// 条尾的展开/收起按钮。
    Expand,
}

/// 右键菜单项;动作与文案由宿主按同一套语义消费。
pub const MENU_ITEMS: [&str; 2] = ["置顶", "降权"];

/// 单条候选的估算宽度:左右留白、序号、文字与注音。
pub fn item_width(item: &Item, metrics: &Metrics, measure: &dyn Measure) -> f32 {
    let s = scale_of(metrics);
    let text_width = measure.width(&item.text, metrics.text);
    let hint_width = if item.hint.is_empty() {
        0.0
    } else {
        4.0 * s + measure.width(&item.hint, metrics.small).min(100.0 * s)
    };
    let hint_gap = if hint_width > 0.0 { 4.0 * s } else { 0.0 };
    // 左留白(10)+序号(12)+间距(4)+候选文字+注音+右留白(10)。
    TEXT_X * s + text_width + hint_gap + hint_width + ITEM_PAD * s
}

/// 超宽截断:按字符收缩到装得下,再补省略号。布局与绘制共用同一份输出。
pub fn ellipsize(text: &str, max_width: f32, size: f32, measure: &dyn Measure) -> String {
    if measure.width(text, size) <= max_width {
        return text.to_string();
    }
    let ell = '…';
    let budget = (max_width - measure.width(&ell.to_string(), size)).max(0.0);
    let mut out = String::new();
    for c in text.chars() {
        let mut next = out.clone();
        next.push(c);
        if measure.width(&next, size) > budget {
            break;
        }
        out = next;
    }
    out.push(ell);
    out
}

/// 当前页在候选列表里的下标区间;页号越界时收进最后一页。
///
/// 固定条与展开棋盘都只画这一区间;引擎的页号、高亮与点选按同一份
/// 页大小换算,翻页后候选与数字键才不会错位。
pub fn page_bounds(page: usize, page_size: usize, len: usize) -> std::ops::Range<usize> {
    let ps = page_size.max(1);
    let pages = len.div_ceil(ps).max(1);
    let start = page.min(pages - 1) * ps;
    start..(start + ps).min(len)
}

/// 棋盘导航的一步:新的高亮与新的滚动起点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridStep {
    pub highlight: usize,
    pub scroll: usize,
}

/// 棋盘可滚动的最大起点:按整行对齐,最后一屏允许不满一行。
pub fn grid_max_scroll(len: usize) -> usize {
    (len.div_ceil(GRID_COLS).saturating_sub(GRID_ROWS)) * GRID_COLS
}

/// 把滚动起点夹进合法范围并向下对齐到整行。
pub fn grid_clamp_scroll(scroll: usize, len: usize) -> usize {
    (scroll / GRID_COLS * GRID_COLS).min(grid_max_scroll(len))
}

/// 打开棋盘时的首屏起点:让高亮落在中间行附近,整行对齐。
pub fn grid_scroll_for(highlight: usize, len: usize) -> usize {
    let row = highlight / GRID_COLS;
    let start = row.saturating_sub(GRID_ROWS / 2);
    (start * GRID_COLS).min(grid_max_scroll(len))
}

/// 棋盘里按格移动;`scroll` 是一屏起点,`len` 是候选总数。
///
/// 每次滚动都推进整行:候选在屏内的列固定不变。右方向键越过末格时,
/// 下一行的第一个候选从底行左端进入,选中的词跟着左移;上下方向键在
/// 顶行/底行同样整行滚,选中格在原列。
pub fn grid_step(highlight: usize, scroll: usize, len: usize, dc: i32, dr: i32) -> GridStep {
    if len == 0 {
        return GridStep {
            highlight: 0,
            scroll: 0,
        };
    }
    let cap = GRID_MAX.min(len);
    let max_scroll = grid_max_scroll(len);
    let scroll = grid_clamp_scroll(scroll, len);
    let highlight = highlight.min(len - 1);
    let stay = GridStep { highlight, scroll };
    let target = if dr > 0 {
        highlight.saturating_add(GRID_COLS)
    } else if dr < 0 {
        if highlight < GRID_COLS {
            return stay;
        }
        highlight - GRID_COLS
    } else if dc > 0 {
        highlight.saturating_add(1)
    } else if dc < 0 {
        if highlight == 0 {
            return stay;
        }
        highlight - 1
    } else {
        return stay;
    };
    if target >= len {
        return stay;
    }
    let scroll = if target < scroll {
        scroll.saturating_sub(GRID_COLS)
    } else if target >= scroll + cap {
        (scroll + GRID_COLS).min(max_scroll)
    } else {
        scroll
    };
    GridStep {
        highlight: target,
        scroll,
    }
}

/// 展开棋盘的滚动条:轨道与滑块;候选不足一屏时返回 None。
/// `hover` 为 true 时展开到 7px,方便鼠标抓取。
pub fn scrollbar_rects(
    scroll: Scroll,
    width: f32,
    height: f32,
    metrics: &Metrics,
    hover: bool,
) -> Option<(Rect, Rect)> {
    if scroll.cap == 0 || scroll.len <= scroll.cap {
        return None;
    }
    let s = scale_of(metrics);
    let bar_w = if hover {
        SCROLLBAR_HOVER
    } else {
        SCROLLBAR_IDLE
    } * s;
    let top = rail_top(metrics);
    let bottom = height - PAD_Y * s;
    let track = Rect::from_xywh(
        rail_center(width, s) - bar_w * 0.5,
        top,
        bar_w,
        (bottom - top).max(0.0),
    );
    let max_scroll = grid_max_scroll(scroll.len).max(1);
    let frac = scroll.offset.min(max_scroll) as f32 / max_scroll as f32;
    let thumb_h = (scroll.cap as f32 / scroll.len as f32 * track.height())
        .max(18.0 * s)
        .min(track.height());
    let thumb_y = track.top + (track.height() - thumb_h) * frac;
    Some((
        track,
        Rect::from_xywh(track.left, thumb_y, track.width(), thumb_h),
    ))
}

/// 滚动条的命中结果:按在滑块上还是轨道空白处。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollHit {
    Thumb,
    Track,
}

/// 滚动条命中:视觉细条外侧有 16px 宽的隐形命中区。
pub fn scrollbar_hit(
    scroll: Scroll,
    width: f32,
    height: f32,
    metrics: &Metrics,
    x: f32,
    y: f32,
) -> Option<ScrollHit> {
    let (track, thumb) = scrollbar_rects(scroll, width, height, metrics, true)?;
    let s = scale_of(metrics);
    // 整条控制轨都可抓;它已在棋盘右侧,不会压住候选格。
    let hit = Rect::from_xywh(rail_left(width, s), track.top, CHEVRON * s, track.height());
    if !hit.contains(x, y) {
        return None;
    }
    Some(if thumb.contains(x, y) {
        ScrollHit::Thumb
    } else {
        ScrollHit::Track
    })
}

/// 滚动条上的鼠标纵坐标 → 滚动起点;`grab` 是拖动时鼠标相对滑块顶部的偏移。
pub fn scrollbar_offset(
    scroll: Scroll,
    width: f32,
    height: f32,
    metrics: &Metrics,
    y: f32,
    grab: Option<f32>,
) -> usize {
    let Some((track, thumb)) = scrollbar_rects(scroll, width, height, metrics, true) else {
        return scroll.offset;
    };
    let travel = (track.height() - thumb.height()).max(1.0);
    let top = match grab {
        Some(grab) => (y - grab).clamp(track.top, track.bottom - thumb.height()),
        None => y - thumb.height() * 0.5,
    };
    let frac = ((top - track.top) / travel).clamp(0.0, 1.0);
    let max_scroll = grid_max_scroll(scroll.len);
    ((frac * max_scroll as f32).round() as usize / GRID_COLS * GRID_COLS).min(max_scroll)
}

/// 固定条能放下几条候选:按自然宽度从左往右排,放不下为止。
/// 数字键最多选到 9,条数也就到 9;单字输入时窄候选多,自然能排上七八条。
pub fn strip_capacity(widths: impl Iterator<Item = f32>, metrics: &Metrics) -> usize {
    let s = scale_of(metrics);
    let usable = STRIP_W * s - (PAD_X + CHEVRON_INSET + CHEVRON + 6.0) * s;
    // 单条候选的固定开销:左右留白、序号与间距。
    let chrome = (TEXT_X + ITEM_PAD) * s;
    let mut count = 0usize;
    let mut used = 0.0f32;
    for text_w in widths {
        let item_w = text_w + chrome;
        if count > 0 && used + GAP * s + item_w > usable {
            break;
        }
        used += if count > 0 { GAP * s } else { 0.0 } + item_w;
        count += 1;
        if count >= 9 {
            break;
        }
    }
    count.max(1)
}

/// 固定条的槽矩形:自然宽度单行从左排,放不下的候选属于下一页。
fn strip_rects(
    spec: &PanelSpec,
    metrics: &Metrics,
    measure: &dyn Measure,
    width: f32,
) -> Vec<Rect> {
    let s = scale_of(metrics);
    let row_h = metrics.visual_row();
    let right = width - (CHEVRON_INSET + CHEVRON + 6.0) * s;
    let mut x = PAD_X * s;
    let mut rects = Vec::with_capacity(spec.items.len());
    for item in &spec.items {
        let item_w = item_width(item, metrics, measure);
        if !rects.is_empty() && x + item_w > right {
            break;
        }
        rects.push(Rect::from_xywh(x, PAD_Y * s, item_w, row_h));
        x += item_w + GAP * s;
    }
    rects
}

/// 右侧控制轨的左侧 x:棋盘到此为止,收起钮与滚动条都落在轨内。
fn rail_left(width: f32, s: f32) -> f32 {
    width - (RAIL_INSET + CHEVRON) * s
}

/// 控制轨中心 x;收起钮圆心与滚动条中线共用。
fn rail_center(width: f32, s: f32) -> f32 {
    rail_left(width, s) + CHEVRON * 0.5 * s
}

/// 滚动条轨道顶部:从收起钮下缘再留 2px。
fn rail_top(metrics: &Metrics) -> f32 {
    let s = scale_of(metrics);
    PAD_Y * s + (metrics.visual_row() - CHEVRON * s) * 0.5 + (CHEVRON + RAIL_GAP) * s
}

/// 棋盘单元的宽度:右侧给控制轨让位,余量在四列间均分。
fn cell_width(width: f32, s: f32) -> f32 {
    (rail_left(width, s) - PAD_X * s - (GRID_COLS as f32 - 1.0) * GAP * s) / GRID_COLS as f32
}

/// 棋盘一格(画布坐标)。
fn cell_rect(width: f32, s: f32, row_h: f32, row: usize, col: usize) -> Rect {
    let w = cell_width(width, s);
    Rect::from_xywh(
        PAD_X * s + col as f32 * (w + GAP * s),
        PAD_Y * s + row as f32 * (row_h + GAP * s),
        w,
        row_h,
    )
}

/// 展开棋盘的全部候选格:按 `items`、`grid` 的顺序一格一条。
/// 四列满格,行与列严格对齐;收起钮在右侧控制轨顶部,不占候选格。
pub fn grid_cells(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> Vec<Rect> {
    let width = panel_width(spec, metrics, measure);
    let s = scale_of(metrics);
    let row_h = metrics.visual_row();
    let count = spec.items.len() + spec.grid.len();
    (0..count)
        .map(|index| cell_rect(width, s, row_h, index / GRID_COLS, index % GRID_COLS))
        .collect()
}

/// 竖排一行一条的矩形。
fn column_rects(spec: &PanelSpec, metrics: &Metrics, width: f32) -> Vec<Rect> {
    let s = scale_of(metrics);
    let row_h = metrics.visual_row();
    let box_width = width - COL_PAD_X * 2.0 * s;
    spec.items
        .iter()
        .enumerate()
        .map(|(index, _)| {
            Rect::from_xywh(
                COL_PAD_X * s,
                (PAD_Y + index as f32 * (row_h + GAP)) * s,
                box_width,
                row_h,
            )
        })
        .collect()
}

/// 候选行的布局矩形(画布坐标);绘制与命中测试共用,所指即所画。
pub fn item_rects(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> Vec<Rect> {
    let width = panel_width(spec, metrics, measure);
    if spec.is_strip() {
        strip_rects(spec, metrics, measure, width)
    } else if matches!(spec.layout, Layout::Column) {
        column_rects(spec, metrics, width)
    } else {
        let cells = grid_cells(spec, metrics, measure);
        cells[..spec.items.len().min(cells.len())].to_vec()
    }
}

/// 展开棋盘的页外候选格;只有展开形态非空。
pub fn grid_rects(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> Vec<Rect> {
    if spec.is_strip() || matches!(spec.layout, Layout::Column) {
        return Vec::new();
    }
    let cells = grid_cells(spec, metrics, measure);
    cells[spec.items.len().min(cells.len())..].to_vec()
}

/// 展开棋盘的右侧控制轨:收起钮与滚动条共用的一条竖槽。
pub fn rail_rect(
    spec: &PanelSpec,
    metrics: &Metrics,
    measure: &dyn Measure,
    height: f32,
) -> Option<Rect> {
    if !spec.expand || matches!(spec.layout, Layout::Column) {
        return None;
    }
    spec.scroll.filter(|scroll| scroll.len > scroll.cap)?;
    let s = scale_of(metrics);
    let cap = chevron_rect(spec, metrics, measure)?;
    Some(Rect::from_xywh(
        cap.left,
        PAD_Y * s,
        cap.width(),
        (height - PAD_Y * 2.0 * s).max(0.0),
    ))
}

/// 条尾 ⌄ 按钮的矩形;竖排没有它。展开态位置不变,成为控制轨的顶帽,
/// 轨道从它下缘向下延伸。
pub fn chevron_rect(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> Option<Rect> {
    if matches!(spec.layout, Layout::Column) {
        return None;
    }
    let width = panel_width(spec, metrics, measure);
    let s = scale_of(metrics);
    let size = CHEVRON * s;
    let x = if spec.expand && matches!(spec.layout, Layout::Wrap) {
        rail_left(width, s)
    } else {
        width - (CHEVRON_INSET + CHEVRON) * s
    };
    let y = PAD_Y * s + (metrics.visual_row() - size) * 0.5;
    Some(Rect::from_xywh(x, y, size, size))
}

/// 条尾箭头的矢量折线(三点);几何居中在按钮矩形里,不依赖字体字形
/// 墨迹在行盒里的位置(⌄ 字形天然偏低)。`expand` 时换成 ⌃。
pub fn chevron_points(rect: Rect, expand: bool, s: f32) -> [Point; 3] {
    let turn = if expand { std::f32::consts::PI } else { 0.0 };
    chevron_points_turn(rect, turn, s)
}

/// 按角度旋转的箭头;展开形变时由 ⌄ 连续转到 ⌃,不跳帧。
pub fn chevron_points_turn(rect: Rect, turn: f32, s: f32) -> [Point; 3] {
    let (cx, cy) = (rect.center_x(), rect.center_y());
    // 展开态的格子是 30px,圆钮更大,箭头按比例放大,免得在大钮里显得小。
    let k = (rect.height() / (CHEVRON * s)).clamp(1.0, 1.4);
    let (a, b) = (4.5 * s * k, 2.5 * s * k);
    let (sin, cos) = turn.sin_cos();
    [(cx - a, cy - b), (cx, cy + b), (cx + a, cy - b)].map(|(x, y)| {
        let (dx, dy) = (x - cx, y - cy);
        Point::new(cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
    })
}

/// 窗口宽度:固定条恒定,展开棋盘也恒定(格子不随候选长短伸缩);
/// 竖排按最宽内容估,翻页时不来回跳。
pub fn panel_width(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> f32 {
    let s = scale_of(metrics);
    if spec.is_strip() {
        return STRIP_W * s;
    }
    // 展开态是棋盘:宽度固定,候选超宽按格截断,窗口不会随内容伸缩。
    if !matches!(spec.layout, Layout::Column) {
        return STRIP_W * s;
    }
    let widest = spec
        .items
        .iter()
        .map(|item| item_width(item, metrics, measure))
        .fold(0.0, f32::max);
    (widest + 18.0 * s)
        .max(spec.width)
        .clamp(360.0 * s, 680.0 * s)
}

/// 窗口高度;公式:`PAD_Y + 行距 ×(行数−1) + 行高 + PAD_Y`。
/// 末行后面没有行间距,所以行区按「行距 ×(行数−1) + 行高」算。
pub fn panel_height(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure) -> f32 {
    let s = scale_of(metrics);
    let rows = layout_rows(spec, metrics, measure);
    if rows == 0 {
        return PAD_Y * 2.0 * s;
    }
    PAD_Y * s + (rows as f32 - 1.0) * metrics.row + metrics.visual_row() + PAD_Y * s
}

/// 候选占几行。固定条恒为一行;竖排一行一条;展开棋盘按格数折算,
/// 四列满格,收起钮浮在首行右端,不占候选格。
pub fn layout_rows(spec: &PanelSpec, _metrics: &Metrics, _measure: &dyn Measure) -> usize {
    if spec.is_strip() {
        return usize::from(!spec.items.is_empty());
    }
    if matches!(spec.layout, Layout::Column) {
        return spec.items.len();
    }
    (spec.items.len() + spec.grid.len()).div_ceil(GRID_COLS)
}

/// 鼠标位置(画布坐标)→ 命中区域。
pub fn hit_test(spec: &PanelSpec, metrics: &Metrics, measure: &dyn Measure, x: f32, y: f32) -> Hit {
    if let Some(rect) = chevron_rect(spec, metrics, measure)
        && rect.contains(x, y)
    {
        return Hit::Expand;
    }
    for (index, rect) in item_rects(spec, metrics, measure).iter().enumerate() {
        if rect.contains(x, y) {
            return Hit::Item(index);
        }
    }
    for (cell, rect) in spec.grid.iter().zip(grid_rects(spec, metrics, measure)) {
        if rect.contains(x, y) {
            return Hit::GridItem(cell.index);
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
    /// 悬停的 ⌄ 按钮。
    pub chev: bool,
    /// 鼠标停在棋盘滚动条上;展开到宽态。
    pub scroll_hover: bool,
}

/// 画一帧候选面板:圆角底、固定条或展开网格。///
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
        canvas.stroke_rrect(
            body,
            PANEL_RADIUS * s,
            s,
            tint(frame.palette.border_strong, 0x4d),
        );
    } else {
        canvas.shadow_rrect(
            Rect::from_xywh(2.0 * s, 3.0 * s, width - 4.0 * s, height - 6.0 * s),
            (PANEL_RADIUS - 1.0) * s,
            16.0 * s,
            Color::with_alpha(0x000000, 56),
        );
        canvas.fill_rrect(body, PANEL_RADIUS * s, Color::opaque(frame.palette.bg));
        canvas.stroke_rrect(
            Rect::from_xywh(
                body.left + 0.5 * s,
                body.top + 0.5 * s,
                (width - 2.0 * s).max(0.0),
                (height - 2.0 * s).max(0.0),
            ),
            (PANEL_RADIUS - 0.5) * s,
            s,
            Color::opaque(frame.palette.border),
        );
    }

    if frame.spec.expand && matches!(frame.spec.layout, Layout::Wrap) {
        // 棋盘:所有候选一样大,文字居中;收起按钮浮在首行右端。
        let cells = grid_cells(frame.spec, &frame.metrics, measure);
        for (index, (item, rect)) in frame.spec.items.iter().zip(&cells).enumerate() {
            draw_cell(
                canvas,
                frame,
                measure,
                &item.text,
                *rect,
                item.selected,
                frame.hover == Some(index),
                glass,
            );
        }
        for (cell, rect) in frame
            .spec
            .grid
            .iter()
            .zip(cells.iter().skip(frame.spec.items.len()))
        {
            draw_cell(
                canvas,
                frame,
                measure,
                &cell.text,
                *rect,
                cell.selected,
                false,
                glass,
            );
        }
    } else {
        let rects = item_rects(frame.spec, &frame.metrics, measure);
        for (index, (item, rect)) in frame.spec.items.iter().zip(&rects).enumerate() {
            draw_item(canvas, frame, measure, item, *rect, index, glass);
        }
        let grid = grid_rects(frame.spec, &frame.metrics, measure);
        for (cell, rect) in frame.spec.grid.iter().zip(&grid) {
            // 网格候选没有序号与注音;文字收进单元矩形,超宽按矩形截断。
            let text = ellipsize(
                &cell.text,
                rect.width() - 8.0 * s,
                frame.metrics.text,
                measure,
            );
            canvas.text(
                &text,
                crate::paint::Point::new(rect.left + 8.0 * s, rect.center_y()),
                frame.metrics.text,
                Color::opaque(frame.palette.text),
                Some(rect.width() - 8.0 * s),
            );
        }
    }

    // 右侧控制轨:一条很淡的竖槽把收起钮和滚动条收进同一件控件。
    if let Some(rail) = rail_rect(frame.spec, &frame.metrics, measure, height) {
        canvas.fill_rrect(rail, rail.width() * 0.5, Color::with_alpha(0x000000, 0x34));
        canvas.stroke_rrect(
            rail,
            rail.width() * 0.5,
            s,
            Color::with_alpha(0xffffff, 0x14),
        );
    }
    if let Some(scroll) = frame.spec.scroll
        && let Some((track, thumb)) =
            scrollbar_rects(scroll, width, height, &frame.metrics, frame.scroll_hover)
    {
        let radius = track.width() * 0.5;
        let (track_a, thumb_a) = if frame.scroll_hover {
            (0x55, 0xdd)
        } else {
            (0x40, 0xbb)
        };
        canvas.fill_rrect(track, radius, tint(frame.palette.text_tertiary, track_a));
        canvas.fill_rrect(thumb, radius, tint(frame.palette.text_tertiary, thumb_a));
    }

    // 条尾箭头:悬停底色 + 矢量折线;展开且有滚动条时缩成顶帽里的小箭头,
    // 与下方轨道合成一根控制轨。
    if let Some(rect) = chevron_rect(frame.spec, &frame.metrics, measure) {
        let expanded = frame.spec.expand && matches!(frame.spec.layout, Layout::Wrap);
        let scrollable = frame
            .spec
            .scroll
            .is_some_and(|scroll| scroll.cap > 0 && scroll.len > scroll.cap);
        if expanded && scrollable {
            if frame.chev {
                canvas.fill_rrect(
                    rect,
                    rect.height() * 0.5,
                    Color::with_alpha(frame.palette.border, 0x66),
                );
            }
        } else if expanded {
            canvas.fill_rrect(
                rect,
                rect.height() * 0.5,
                Color::with_alpha(frame.palette.border, if frame.chev { 0x66 } else { 0x38 }),
            );
        } else if frame.chev {
            canvas.fill_rrect(rect, rect.height() * 0.5, tint(frame.palette.border, 0x4d));
        }
        let glyph = if expanded && scrollable {
            let size = 14.0 * s;
            Rect::from_xywh(
                rect.center_x() - size * 0.5,
                rect.center_y() - size * 0.5,
                size,
                size,
            )
        } else {
            rect
        };
        let alpha = if expanded && scrollable && !frame.chev {
            0x8c
        } else {
            0xff
        };
        let (color, width) = (
            Color::with_alpha(frame.palette.text_secondary, alpha),
            1.6 * s,
        );
        let turn = if frame.spec.expand {
            std::f32::consts::PI
        } else {
            0.0
        };
        let [a, b, c] = chevron_points_turn(glyph, turn, s);
        canvas.line(a, b, width, color);
        canvas.line(b, c, width, color);
    }
}

/// 选中槽:整行高的胶囊,上亮下深的釉面 + 贴边镜面 + 底缘釉根。
/// 固定条、竖排与棋盘共用;形状与平台无关,材质由 glass 决定。
fn draw_selection(canvas: &mut dyn Canvas, frame: &Frame<'_>, rect: Rect, glass: bool) {
    let s = frame.metrics.scale();
    let radius = rect.height() * 0.5;
    let (top, bottom) = selection_stops(frame.palette.selected_bg, glass);
    canvas.fill_rrect_vertical(rect, radius, Color::opaque(top), Color::opaque(bottom));
    let (shine, root) = if glass {
        (0xfff3cf, 0x6b4300)
    } else {
        (0xffffff, 0x000000)
    };
    let top_y = rect.top + 1.5 * s;
    let inset = capsule_inset(rect, top_y) + s;
    canvas.line(
        Point::new(rect.left + inset, top_y),
        Point::new(rect.right - inset, top_y),
        s,
        Color::with_alpha(shine, if glass { 0x66 } else { 0x2e }),
    );
    let bottom_y = rect.bottom - 1.5 * s;
    let inset = capsule_inset(rect, bottom_y) + s;
    canvas.line(
        Point::new(rect.left + inset, bottom_y),
        Point::new(rect.right - inset, bottom_y),
        s,
        Color::with_alpha(root, if glass { 0x4d } else { 0x26 }),
    );
}

/// 棋盘的一格:选中时垫同款黄釉胶囊,文字在格内居中。
/// 棋盘不画序号与注音,所有格保持同一套排印,列与列才对得齐。
#[allow(clippy::too_many_arguments)]
fn draw_cell(
    canvas: &mut dyn Canvas,
    frame: &Frame<'_>,
    measure: &dyn Measure,
    text: &str,
    rect: Rect,
    selected: bool,
    hovered: bool,
    glass: bool,
) {
    let s = frame.metrics.scale();
    if selected {
        draw_selection(canvas, frame, rect, glass);
    } else if hovered {
        let hover = if glass {
            Color::with_alpha(frame.palette.hover, 0x66)
        } else {
            Color::opaque(frame.palette.hover)
        };
        canvas.fill_rrect(rect, rect.height() * 0.5, hover);
    }
    let budget = (rect.width() - 16.0 * s).max(1.0);
    let shown = ellipsize(text, budget, frame.metrics.text, measure);
    canvas.text_centered(
        &shown,
        Rect::from_xywh(rect.left + 8.0 * s, rect.top, budget, rect.height()),
        frame.metrics.text,
        Color::opaque(if selected {
            frame.palette.selected_text
        } else {
            frame.palette.text
        }),
    );
}

/// 画一条候选:选中胶囊、序号与文字;固定条按条尾边界截断,
/// 一条候选占满整条时也不会越过窗口。
fn draw_item(
    canvas: &mut dyn Canvas,
    frame: &Frame<'_>,
    measure: &dyn Measure,
    item: &Item,
    rect: Rect,
    index: usize,
    glass: bool,
) {
    let s = frame.metrics.scale();
    let tint = |rgb: u32, alpha: u8| {
        if glass {
            Color::with_alpha(rgb, alpha)
        } else {
            Color::opaque(rgb)
        }
    };
    if item.selected {
        draw_selection(canvas, frame, rect, glass);
    } else if frame.hover == Some(index) {
        canvas.fill_rrect(rect, rect.height() * 0.5, tint(frame.palette.hover, 0x66));
    }
    canvas.text(
        &format!("{}", index + 1),
        Point::new(rect.left + ITEM_PAD * s, rect.center_y()),
        frame.metrics.small,
        Color::opaque(if item.selected {
            frame.palette.accent_text
        } else {
            frame.palette.text_tertiary
        }),
        Some(20.0 * s),
    );
    let text_x = rect.left + TEXT_X * s;
    // 固定条:截到条尾;展开与竖排:整词,max_text_width 兜底。
    let budget = if frame.spec.is_strip() {
        let width = panel_width(frame.spec, &frame.metrics, measure);
        (width - PAD_X * s - text_x - ITEM_PAD * s).max(1.0)
    } else {
        frame.spec.max_text_width.max(1.0)
    };
    let text = ellipsize(&item.text, budget, frame.metrics.text, measure);
    canvas.text(
        &text,
        crate::paint::Point::new(text_x, rect.center_y()),
        frame.metrics.text,
        Color::opaque(if item.selected {
            frame.palette.selected_text
        } else {
            frame.palette.text
        }),
        Some(budget),
    );
    if !frame.spec.is_strip() && !item.hint.is_empty() {
        let text_width = measure.width(&text, frame.metrics.text);
        canvas.text(
            &item.hint,
            crate::paint::Point::new(text_x + text_width + 4.0 * s, rect.center_y()),
            frame.metrics.small,
            Color::opaque(frame.palette.text_tertiary),
            Some(100.0 * s),
        );
    }
}
