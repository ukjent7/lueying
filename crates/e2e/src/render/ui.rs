//! 模式徽标与选择面板的布局、命中与绘制指令 E2E。
//!
//! 绘制端口把画面描述成指令,这一层断言几何、绘制次序与配色;Direct2D
//! 画出来的真实像素由 Windows 侧的自检覆盖,Linux CI 不需要光栅后端。

use lueying_render::Palette;
use lueying_render::badge;
use lueying_render::paint::{Canvas, Color, Point};
use lueying_render::panel::{Estimate, Measure, Rect};
use lueying_render::picker;

use crate::harness::{palette, push};

/// 一条绘制指令。
#[derive(Clone, Debug, PartialEq)]
enum Op {
    Rect(Rect, Color),
    StrokeRect(Rect, f32, Color),
    RRect(Rect, f32, Color),
    StrokeRRect(Rect, f32, f32, Color),
    Shadow(Rect, f32, f32, Color),
    Text {
        content: String,
        origin: Point,
        size: f32,
        color: Color,
        max_width: Option<f32>,
    },
    TextCentered {
        content: String,
        rect: Rect,
        size: f32,
        color: Color,
    },
}

/// 记录画布:按绘制顺序收集指令;测宽用与布局相同的估算实现。
struct Recorder {
    ops: Vec<Op>,
    limit: Rect,
}

impl Recorder {
    fn new(width: f32, height: f32) -> Self {
        Self {
            ops: Vec::new(),
            limit: Rect::from_xywh(0.0, 0.0, width, height),
        }
    }

    /// 某个颜色的圆角矩形及其圆角半径。
    fn rrects(&self, color: u32) -> Vec<(&Rect, f32)> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Op::RRect(rect, radius, c) if c.rgb == color && c.alpha == 255 => {
                    Some((rect, *radius))
                }
                _ => None,
            })
            .collect()
    }

    /// 内容完全一致的文字指令。
    fn text(&self, content: &str) -> Option<&Op> {
        self.ops.iter().find(|op| match op {
            Op::Text { content: c, .. } | Op::TextCentered { content: c, .. } => c == content,
            _ => false,
        })
    }

    /// 每条矩形指令都落在画布内;投影按设计会溢出卡片,不参与。
    fn inside(&self) -> bool {
        self.ops.iter().all(|op| {
            let rect = match op {
                Op::Rect(rect, _)
                | Op::StrokeRect(rect, ..)
                | Op::RRect(rect, ..)
                | Op::StrokeRRect(rect, ..)
                | Op::TextCentered { rect, .. } => *rect,
                Op::Shadow(..) | Op::Text { .. } => return true,
            };
            rect.left >= self.limit.left - 0.01
                && rect.top >= self.limit.top - 0.01
                && rect.right <= self.limit.right + 0.01
                && rect.bottom <= self.limit.bottom + 0.01
        })
    }
}

impl Canvas for Recorder {
    fn fill_rect(&mut self, rect: Rect, color: Color) {
        self.ops.push(Op::Rect(rect, color));
    }

    fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color) {
        self.ops.push(Op::StrokeRect(rect, width, color));
    }

    fn fill_rrect(&mut self, rect: Rect, radius: f32, color: Color) {
        self.ops.push(Op::RRect(rect, radius, color));
    }

    fn stroke_rrect(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        self.ops.push(Op::StrokeRRect(rect, radius, width, color));
    }

    fn shadow_rrect(&mut self, rect: Rect, radius: f32, blur: f32, color: Color) {
        self.ops.push(Op::Shadow(rect, radius, blur, color));
    }

    fn text(
        &mut self,
        content: &str,
        origin: Point,
        size: f32,
        color: Color,
        max_width: Option<f32>,
    ) {
        self.ops.push(Op::Text {
            content: content.to_string(),
            origin,
            size,
            color,
            max_width,
        });
    }

    fn measure(&mut self, content: &str, size: f32) -> f32 {
        let estimate = Estimate;
        estimate.width(content, size)
    }

    fn text_centered(&mut self, content: &str, rect: Rect, size: f32, color: Color) {
        self.ops.push(Op::TextCentered {
            content: content.to_string(),
            rect,
            size,
            color,
        });
    }
}

/// 面板底色:第一条指令铺满整个画布,窗口不会露出系统背景。
fn background(recorder: &Recorder, palette: &Palette) -> bool {
    matches!(
        recorder.ops.first(),
        Some(Op::Rect(rect, color))
            if *rect == recorder.limit && color.rgb == palette.bg && color.alpha == 255
    )
}

/// 选择面板:尺寸、命中、绘制次序、选中行底色与长标题截断。
/// 宿主选择面板:尺寸、行命中、底色、选中行与空状态。
fn picker_checks(checks: &mut Vec<(&'static str, bool, String)>) {
    let palette = palette();
    let spec = picker::Spec {
        title: "选择语言".into(),
        rows: vec!["中文".into(), "English".into(), "日本語".into()],
        selected: 1,
        total: 3,
        above: 0,
        below: 0,
    };
    let layout = picker::layout(&spec, 1.0);
    let expect_h = picker::TITLE_H + 3.0 * picker::ROW_H + picker::FOOTER_H;
    push(
        checks,
        "选择面板尺寸",
        (layout.size.0 - picker::WIDTH).abs() < 0.5 && (layout.size.1 - expect_h).abs() < 0.5,
        format!("{:?} 期望 ({}, {expect_h})", layout.size, picker::WIDTH),
    );
    let row = layout.rows[1];
    let row_hit = picker::hit(&layout, &spec, row.center_x(), row.center_y());
    push(
        checks,
        "选择面板行命中",
        row_hit == picker::Hit::Row(1),
        format!("{row_hit:?}"),
    );
    let outside = picker::hit(&layout, &spec, picker::WIDTH + 10.0, 10.0);
    push(
        checks,
        "选择面板外部不命中",
        outside == picker::Hit::Nothing,
        format!("{outside:?}"),
    );

    let mut recorder = Recorder::new(layout.size.0, layout.size.1);
    picker::draw(&mut recorder, &spec, &layout, &palette, 1.0);
    push(
        checks,
        "选择面板底色",
        background(&recorder, &palette),
        format!("首条指令 {:?}", recorder.ops.first()),
    );
    let selected = Rect::from_ltrb(row.left, row.top + 2.0, row.right, row.bottom - 2.0);
    let rrects = recorder.rrects(palette.selected_bg);
    push(
        checks,
        "选择面板选中行底色",
        rrects.len() == 1 && *rrects[0].0 == selected,
        format!("{rrects:?} 期望 {selected:?}"),
    );
    push(
        checks,
        "选择面板标题与选中行文字",
        recorder.text("选择语言").is_some()
            && matches!(
                recorder.text("English"),
                Some(Op::Text { color, .. }) if color.rgb == palette.selected_text
            ),
        "标题与选中行各有一条文字指令".into(),
    );
    push(
        checks,
        "选择面板绘制不越界",
        recorder.inside(),
        format!("{} 条指令", recorder.ops.len()),
    );

    let empty = picker::Spec {
        title: "选择".into(),
        total: 0,
        ..picker::Spec::default()
    };
    let empty_layout = picker::layout(&empty, 1.0);
    let expect_empty = picker::TITLE_H + picker::ROW_H + picker::FOOTER_H;
    push(
        checks,
        "选择面板空状态尺寸",
        (empty_layout.size.1 - expect_empty).abs() < 0.5,
        format!("{:.1} 期望 {expect_empty:.1}", empty_layout.size.1),
    );
    let mut empty_recorder = Recorder::new(empty_layout.size.0, empty_layout.size.1);
    picker::draw(&mut empty_recorder, &empty, &empty_layout, &palette, 1.0);
    push(
        checks,
        "选择面板空状态提示",
        empty_recorder.text("没有可选项").is_some(),
        format!("{:?}", empty_recorder.ops.len()),
    );
}

/// 模式徽标:卡片底色、圆角、投影与淡入淡出的透明度。
fn badge_checks(checks: &mut Vec<(&'static str, bool, String)>) {
    let palette = palette();
    let width = badge::WIDTH;
    let height = badge::HEIGHT;
    let card = Rect::from_xywh(0.5, 0.5, width - 1.0, height - 1.0);
    let mut recorder = Recorder::new(width, height);
    badge::draw(
        &mut recorder,
        &badge::BadgeSpec {
            label: "中",
            palette: &palette,
            width,
            height,
            scale: 1.0,
            opacity: 1.0,
            shadow: false,
        },
    );
    let rrects = recorder.rrects(palette.bg);
    push(
        checks,
        "徽标卡片底色",
        rrects.len() == 1 && *rrects[0].0 == card && rrects[0].1 > 0.0,
        format!("{rrects:?} 期望 {card:?}"),
    );
    push(
        checks,
        "徽标文字居中",
        matches!(
            recorder.text("中"),
            Some(Op::TextCentered { rect, color, .. })
                if *rect == card && color.rgb == palette.text
        ),
        "标签在卡片矩形内居中且用正文色".into(),
    );
    push(
        checks,
        "徽标绘制不越界",
        recorder.inside(),
        format!("{} 条指令", recorder.ops.len()),
    );

    let mut fading = Recorder::new(width, height);
    badge::draw(
        &mut fading,
        &badge::BadgeSpec {
            label: "英",
            palette: &palette,
            width,
            height,
            scale: 1.0,
            opacity: 0.5,
            shadow: false,
        },
    );
    let faded = fading.ops.iter().all(|op| match op {
        Op::RRect(_, _, color)
        | Op::StrokeRRect(_, _, _, color)
        | Op::Shadow(_, _, _, color)
        | Op::TextCentered { color, .. } => color.alpha == 128,
        _ => false,
    });
    push(
        checks,
        "徽标淡出统一透明度",
        faded && !fading.ops.is_empty(),
        format!("{:?}", fading.ops.len()),
    );

    // 高对比度主题不画投影;投影只由一次指令给出,由后端展开。
    let mut plain = Recorder::new(width, height);
    badge::draw(
        &mut plain,
        &badge::BadgeSpec {
            label: "全",
            palette: &palette,
            width,
            height,
            scale: 1.0,
            opacity: 1.0,
            shadow: true,
        },
    );
    push(
        checks,
        "徽标投影",
        plain
            .ops
            .iter()
            .any(|op| matches!(op, Op::Shadow(rect, ..) if *rect == card)),
        format!("{} 条指令", plain.ops.len()),
    );
}

pub(crate) fn ui_checks(checks: &mut Vec<(&'static str, bool, String)>) {
    badge_checks(checks);
    picker_checks(checks);
}
