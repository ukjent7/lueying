//! 模式徽标、设置窗控件层与命令面板的布局、命中与像素 E2E。

use lueying_render::badge;
use lueying_render::command;
use lueying_render::ui;
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

/// 设置窗的样例页面:一个分组,三种控件各来一个。
fn settings_page() -> ui::Page {
    ui::Page {
        title: "掠影".into(),
        subtitle: "拼音输入法 · v0.1.0".into(),
        search: String::new(),
        header_button: Some(ui::Button {
            id: ui::Id::new("update", 0),
            text: "检查更新".into(),
            kind: ui::ButtonKind::Ghost,
            enabled: true,
        }),
        nav: ["输入", "候选", "扩展", "词库", "关于"]
            .iter()
            .enumerate()
            .map(|(index, text)| ui::NavItem {
                id: ui::Id::new("nav", index),
                text: text.to_string(),
            })
            .collect(),
        active_nav: 0,
        groups: vec![ui::Group {
            title: "输入".into(),
            rows: vec![
                ui::Row {
                    label: "辅助码".into(),
                    detail: "拼音后按 ` 再打 1–2 位五笔码".into(),
                    control: ui::Control::Toggle {
                        id: ui::Id::new("aux", 0),
                        on: true,
                    },
                },
                ui::Row {
                    label: "输入方案".into(),
                    detail: "全拼或双拼,共享词库".into(),
                    control: ui::Control::Segment {
                        id: ui::Id::new("scheme", 0),
                        options: vec![
                            ui::SegmentOption::new("全拼"),
                            ui::SegmentOption::new("双拼"),
                        ],
                        selected: 0,
                        width: 44.0,
                    },
                },
                ui::Row {
                    label: "候选窗预览".into(),
                    detail: "改字号、排列或主题色会实时反映".into(),
                    control: ui::Control::Preview {
                        id: ui::Id::new("candidate-preview", 0),
                        style: ui::CandidatePreview {
                            accent: 0x2f6fed,
                            font_scale: 1.0,
                            vertical: false,
                            page_size: 5,
                            pinyin: "lue ying".into(),
                            candidates: vec![
                                "掠影".into(),
                                "略影".into(),
                                "掠应".into(),
                                "月影".into(),
                                "掠映".into(),
                            ],
                        },
                    },
                },
                ui::Row {
                    label: "自定义短语".into(),
                    detail: "编辑 phrases.txt,保存后重载".into(),
                    control: ui::Control::Buttons {
                        items: vec![ui::Button {
                            id: ui::Id::new("open-phrases", 0),
                            text: "编辑".into(),
                            kind: ui::ButtonKind::Secondary,
                            enabled: true,
                        }],
                    },
                },
                ui::Row {
                    label: "用户效果".into(),
                    detail: "SkSL 装饰层,放进去即生效".into(),
                    control: ui::Control::ButtonsToggle {
                        items: vec![ui::Button {
                            id: ui::Id::new("open-effects", 0),
                            text: "目录".into(),
                            kind: ui::ButtonKind::Secondary,
                            enabled: true,
                        }],
                        toggle: ui::Id::new("effects", 0),
                        toggle_on: true,
                    },
                },
            ],
        }],
        notice: "修改后点保存生效".into(),
        notice_tint: 0x5f636b,
        footer_buttons: vec![
            ui::Button {
                id: ui::Id::new("close", 0),
                text: "关闭".into(),
                kind: ui::ButtonKind::Secondary,
                enabled: true,
            },
            ui::Button {
                id: ui::Id::new("save", 0),
                text: "保存".into(),
                kind: ui::ButtonKind::Primary,
                enabled: true,
            },
        ],
    }
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

    // 设置窗控件层:布局、命中与像素。
    let font = ui_font();
    let page = settings_page();
    let layout = ui::layout(&page, &font, 1.0, 650.0);
    let nav = layout
        .page_hit(ui::Id::new("nav", 1))
        .expect("左栏第二项矩形");
    let nav_hit = layout.hit_page(center_x(nav), center_y(nav));
    push(
        checks,
        "设置左栏命中",
        nav_hit == Some(ui::Id::new("nav", 1)),
        format!("{nav_hit:?}"),
    );
    // 命中:开关、分段格、按钮各归各位;空白处不命中。
    let toggle = layout.card_hit(ui::Id::new("aux", 0)).expect("开关矩形");
    let hit = layout.hit_card(center_x(toggle), center_y(toggle));
    push(
        checks,
        "设置开关命中",
        hit == Some(ui::Id::new("aux", 0)),
        format!("{hit:?}"),
    );
    let cell = layout
        .card_hit(ui::Id::new("scheme", 1))
        .expect("分段格矩形");
    let hit = layout.hit_card(center_x(cell), center_y(cell));
    push(
        checks,
        "设置分段命中",
        hit == Some(ui::Id::new("scheme", 1)),
        format!("{hit:?}"),
    );
    let button = layout
        .card_hit(ui::Id::new("open-phrases", 0))
        .expect("按钮矩形");
    let hit = layout.hit_card(center_x(button), center_y(button));
    push(
        checks,
        "设置按钮命中",
        hit == Some(ui::Id::new("open-phrases", 0)),
        format!("{hit:?}"),
    );
    let hit = layout.hit_card(4.0, 4.0);
    push(checks, "设置空白不命中", hit.is_none(), format!("{hit:?}"));
    // 控件右对齐在行内,不出卡片(卡片坐标)。
    let aligned = (toggle.right - (layout.card.width() - 14.0)).abs() < 0.5;
    let inside = toggle.right <= layout.card.width() && toggle.left >= 0.0;
    push(
        checks,
        "设置控件右对齐",
        aligned && inside,
        format!("右边距={:.1}", layout.card.right - toggle.right),
    );
    // 像素:窗口底色、开关闭合的旋钮、选中分段的底色。
    let mut ui_surface =
        surfaces::raster_n32_premul((ui::PAGE_WIDTH as i32, 650)).expect("raster surface");
    ui::Painter::new(ui_surface.canvas(), &palette(), &font, 1.0).paint(&page, &layout, None, 0.0);
    let corner = read_bgra(&mut ui_surface, 2, 2);
    push(
        checks,
        "设置窗底色",
        corner.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette().window_bg)),
        format!("{corner:?} 期望 {:?}", rgb(palette().window_bg)),
    );
    let knob_x = (layout.card.left + toggle.right - 3.0 - 8.0) as i32;
    let knob_y = (layout.card.top + center_y(toggle)) as i32;
    let knob = read_bgra(&mut ui_surface, knob_x, knob_y);
    push(
        checks,
        "设置开关旋钮",
        knob.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette().on_accent)),
        format!("{knob:?} 期望 {:?}", rgb(palette().on_accent)),
    );
    // 选中格的底色在格子左缘(避开居中的文字);只有选中的那格是白的,
    // 没选中的格子露的是分段轨道底色。
    let selected_cell = layout
        .card_hit(ui::Id::new("scheme", 0))
        .expect("选中分段格矩形");
    let (cell_x, cell_y) = (
        (layout.card.left + selected_cell.left + 2.0) as i32,
        (layout.card.top + center_y(selected_cell)) as i32,
    );
    let cell_pixel = read_bgra(&mut ui_surface, cell_x, cell_y);
    push(
        checks,
        "设置选中分段底色",
        cell_pixel.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette().segment)),
        format!("{cell_pixel:?} 期望 {:?}", rgb(palette().segment)),
    );
    // 页脚贴视口底:内容比视口矮时页脚也在底上。
    let footer_bottom = layout.footer.bottom;
    push(
        checks,
        "设置页脚贴底",
        (footer_bottom - (650.0 - 18.0)).abs() < 0.5,
        format!("页脚下沿={footer_bottom:.1}"),
    );
    // 视口够高时卡片不滚动。
    let tall = ui::layout(&page, &font, 1.0, 2000.0);
    push(
        checks,
        "设置视口够高不滚动",
        tall.max_scroll() == 0.0,
        format!("max_scroll={:.1}", tall.max_scroll()),
    );
    // 视口矮时卡片能滚,滚动量等于多出来的高度。
    let short = ui::layout(&page, &font, 1.0, 200.0);
    let expect = (short.card.bottom - short.scroll.bottom).max(0.0);
    push(
        checks,
        "设置视口矮时可滚动",
        (short.max_scroll() - expect).abs() < 0.5 && expect > 0.0,
        format!("max_scroll={:.1} 期望 {expect:.1}", short.max_scroll()),
    );
    // 按钮与开关并排:开关贴行右,按钮在它左边,两者各自命中。
    let effect_toggle = layout
        .card_hit(ui::Id::new("effects", 0))
        .expect("效果开关矩形");
    let effect_button = layout
        .card_hit(ui::Id::new("open-effects", 0))
        .expect("效果按钮矩形");
    let hit = layout.hit_card(center_x(effect_toggle), center_y(effect_toggle));
    let hit_button = layout.hit_card(center_x(effect_button), center_y(effect_button));
    push(
        checks,
        "设置按钮开关并排",
        hit == Some(ui::Id::new("effects", 0))
            && hit_button == Some(ui::Id::new("open-effects", 0))
            && (effect_button.right <= effect_toggle.left)
            && (effect_toggle.right - toggle.right).abs() < 0.5,
        format!("开关={hit:?} 按钮={hit_button:?}"),
    );

    // 顶部搜索框:几何留在标题与更新按钮之间,底色与放大镜画出容器。
    let search = layout.search;
    push(
        checks,
        "设置搜索框几何",
        search.width() >= 120.0
            && search.left > layout.logo.right
            && layout
                .page_hit(ui::Id::new("update", 0))
                .is_some_and(|button| search.right <= button.left),
        format!("search={search:?} logo_right={:.1}", layout.logo.right),
    );
    let search_pixel = read_bgra(
        &mut ui_surface,
        (search.left + 3.0) as i32,
        (search.top + search.height() / 2.0) as i32,
    );
    push(
        checks,
        "设置搜索框底色",
        search_pixel.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette().surface_alt)),
        format!("{search_pixel:?} 期望 {:?}", rgb(palette().surface_alt)),
    );
    // 候选窗预览行比普通行高:去掉预览行后卡片应矮一截。
    let mut no_preview = page.clone();
    no_preview.groups[0]
        .rows
        .retain(|row| !matches!(row.control, ui::Control::Preview { .. }));
    let short_layout = ui::layout(&no_preview, &font, 1.0, 650.0);
    let saved = layout.card.height() - short_layout.card.height();
    push(
        checks,
        "设置候选预览预留画布",
        saved > 60.0 && layout.height > short_layout.height,
        format!("预览行高差 {saved:.1}"),
    );

    command_checks(checks);
}
