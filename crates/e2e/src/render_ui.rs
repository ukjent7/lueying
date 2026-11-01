//! 模式徽标、命令面板与选择面板的布局、命中与像素 E2E。

use lueying_render::badge;
use lueying_render::command;
use lueying_render::picker;
use skia_safe::{Surface, surfaces};

use crate::harness::{palette, push, read_bgra, rgb};

/// 矩形中心横坐标。
fn center_x(rect: skia_safe::Rect) -> f32 {
    (rect.left + rect.right) / 2.0
}

/// 矩形中心纵坐标。
fn center_y(rect: skia_safe::Rect) -> f32 {
    (rect.top + rect.bottom) / 2.0
}

/// 设置窗用的字型;系统没有字型时退回默认字型(只影响宽度测量)。
fn ui_font() -> skia_safe::Font {
    let mgr = skia_safe::FontMgr::new();
    match mgr.legacy_make_typeface(None, skia_safe::FontStyle::normal()) {
        Some(face) => skia_safe::Font::from_typeface(face, 17.0),
        None => skia_safe::Font::default(),
    }
}

/// 命令面板:尺寸、命中、选中行底色、空状态与长标题截断。
fn command_checks(checks: &mut Vec<(&'static str, bool, String)>) {
    let palette = palette();
    let font = ui_font();
    let spec = command::Spec {
        query: "xue".into(),
        rows: vec![
            command::Row {
                title: "OCR 取字".into(),
                extension: "demo".into(),
            },
            command::Row {
                title: "学习".into(),
                extension: "demo".into(),
            },
            command::Row {
                title: "工作".into(),
                extension: "demo".into(),
            },
        ],
        selected: 1,
        total: 3,
        above: 0,
        below: 0,
    };
    let layout = command::layout(&spec, 1.0);
    let expect_h = command::INPUT_H + 3.0 * command::ROW_H + command::FOOTER_H;
    push(
        checks,
        "命令面板尺寸",
        (layout.size.0 - command::WIDTH).abs() < 0.5 && (layout.size.1 - expect_h).abs() < 0.5,
        format!("{:?} 期望 ({}, {expect_h})", layout.size, command::WIDTH),
    );
    let input_hit = command::hit(&layout, &spec, command::WIDTH / 2.0, command::INPUT_H / 2.0);
    push(
        checks,
        "命令面板输入框命中",
        input_hit == command::Hit::Input,
        format!("{input_hit:?}"),
    );
    let row = layout.rows[1];
    let row_hit = command::hit(&layout, &spec, center_x(row), center_y(row));
    push(
        checks,
        "命令面板行命中",
        row_hit == command::Hit::Row(1),
        format!("{row_hit:?}"),
    );
    let footer_hit = command::hit(
        &layout,
        &spec,
        command::WIDTH / 2.0,
        layout.footer.center_y(),
    );
    push(
        checks,
        "命令面板页脚命中",
        footer_hit == command::Hit::Footer,
        format!("{footer_hit:?}"),
    );
    let outside = command::hit(&layout, &spec, command::WIDTH + 10.0, 10.0);
    push(
        checks,
        "命令面板外部不命中",
        outside == command::Hit::Nothing,
        format!("{outside:?}"),
    );

    let mut surface =
        surfaces::raster_n32_premul((layout.size.0 as i32, layout.size.1 as i32)).expect("raster");
    command::draw(surface.canvas(), &spec, &layout, &palette, &font, 1.0);
    let corner = read_bgra(&mut surface, 2, 2);
    push(
        checks,
        "命令面板底色",
        corner.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette.bg)),
        format!("{corner:?} 期望 {:?}", rgb(palette.bg)),
    );
    let selected = layout.rows[1];
    let pixel = read_bgra(
        &mut surface,
        selected.left as i32 + 4,
        selected.center_y() as i32,
    );
    push(
        checks,
        "命令面板选中行底色",
        pixel.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette.selected_bg)),
        format!("{pixel:?} 期望 {:?}", rgb(palette.selected_bg)),
    );

    // 空状态仍占一行高度;提示语画在列表区。
    let empty = command::Spec {
        query: "zzz".into(),
        total: 0,
        ..command::Spec::default()
    };
    let empty_layout = command::layout(&empty, 1.0);
    let expect_empty = command::INPUT_H + command::ROW_H + command::FOOTER_H;
    push(
        checks,
        "命令面板空状态尺寸",
        (empty_layout.size.1 - expect_empty).abs() < 0.5,
        format!("{:.1} 期望 {expect_empty:.1}", empty_layout.size.1),
    );

    // 长标题按行宽截断,卷动提示不影响几何。
    let long = command::Spec {
        query: "a".into(),
        rows: vec![command::Row {
            title: "很长的命令标题".repeat(20),
            extension: "very-long-extension-name".into(),
        }],
        selected: 0,
        total: 9,
        above: 3,
        below: 5,
    };
    let long_layout = command::layout(&long, 1.5);
    let mut long_surface =
        surfaces::raster_n32_premul((long_layout.size.0 as i32, long_layout.size.1 as i32))
            .expect("raster");
    command::draw(
        long_surface.canvas(),
        &long,
        &long_layout,
        &palette,
        &font,
        1.5,
    );
    push(
        checks,
        "命令面板长标题不越界",
        long_layout.rows.len() == 1 && long_layout.rows[0].right <= long_layout.size.0,
        format!("{:?}", long_layout.rows),
    );
}

/// 宿主选择面板:尺寸、行命中、选中底色与空状态。
fn picker_checks(checks: &mut Vec<(&'static str, bool, String)>) {
    let palette = palette();
    let font = ui_font();
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
    let row_hit = picker::hit(&layout, &spec, center_x(row), center_y(row));
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
    let mut surface =
        surfaces::raster_n32_premul((layout.size.0 as i32, layout.size.1 as i32)).expect("raster");
    picker::draw(surface.canvas(), &spec, &layout, &palette, &font, 1.0);
    let corner = read_bgra(&mut surface, 2, 2);
    push(
        checks,
        "选择面板底色",
        corner.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette.bg)),
        format!("{corner:?} 期望 {:?}", rgb(palette.bg)),
    );
    let pixel = read_bgra(&mut surface, row.left as i32 + 4, row.center_y() as i32);
    push(
        checks,
        "选择面板选中行底色",
        pixel.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette.selected_bg)),
        format!("{pixel:?} 期望 {:?}", rgb(palette.selected_bg)),
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
}

pub(crate) fn ui_checks(checks: &mut Vec<(&'static str, bool, String)>) {
    // 模式徽标:卡片底色、圆角外的透明,以及字型有没有真的画出来。
    let (bw, bh) = (badge::WIDTH as i32, badge::HEIGHT as i32);
    let plain_badge = |surface: &mut Surface, font: Option<&skia_safe::Font>| {
        badge::draw(
            surface.canvas(),
            &badge::BadgeSpec {
                label: "中",
                palette: &palette(),
                font,
                width: badge::WIDTH,
                height: badge::HEIGHT,
                scale: 1.0,
                shadow: false,
            },
        );
    };
    let mut badge_surface = surfaces::raster_n32_premul((bw, bh)).expect("raster surface");
    plain_badge(&mut badge_surface, None);
    match read_bgra(&mut badge_surface, bw / 2, bh / 2) {
        Some(pixel) => push(
            checks,
            "徽标卡片底色",
            pixel.3 == 255 && (pixel.0, pixel.1, pixel.2) == rgb(palette().bg),
            format!("{pixel:?} 期望 {:?} 全不透明", rgb(palette().bg)),
        ),
        None => push(checks, "徽标卡片底色", false, "像素读回失败".into()),
    }
    match read_bgra(&mut badge_surface, 0, 0) {
        Some(pixel) => push(
            checks,
            "徽标圆角外透明",
            pixel.3 == 0,
            format!("{pixel:?} 期望 alpha=0"),
        ),
        None => push(checks, "徽标圆角外透明", false, "像素读回失败".into()),
    }
    // 字型:系统一个可用字型都没有是环境问题,跳过;有字型却画不出东西才是 bug。
    let mgr = skia_safe::FontMgr::new();
    let families = mgr.count_families();
    match mgr.legacy_make_typeface(None, skia_safe::FontStyle::normal()) {
        Some(face) => {
            let font = skia_safe::Font::from_typeface(face, 17.0);
            let mut with_font = surfaces::raster_n32_premul((bw, bh)).expect("raster surface");
            plain_badge(&mut with_font, Some(&font));
            let mut plain = surfaces::raster_n32_premul((bw, bh)).expect("raster surface");
            plain_badge(&mut plain, None);
            let mut differs = false;
            'y: for y in 0..bh {
                for x in 0..bw {
                    if read_bgra(&mut with_font, x, y) != read_bgra(&mut plain, x, y) {
                        differs = true;
                        break 'y;
                    }
                }
            }
            // 一个字体族都没有时,字型画不出东西是环境问题,不算渲染 bug。
            let ok = differs || families == 0;
            push(
                checks,
                "徽标字型出图",
                ok,
                format!("与无字型画面不同={differs} 字体族={families}"),
            );
        }
        None => push(checks, "徽标字型出图", true, "系统没有可用字型,跳过".into()),
    }

    command_checks(checks);
    picker_checks(checks);
}
