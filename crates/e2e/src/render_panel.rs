//! 候选面板 E2E:底色、布局行数、窗口高度公式与命中测试。

use lueying_render::panel::{self, Item, Layout, Metrics};
use skia_safe::surfaces;

use crate::harness::{palette, push, read_bgr, rgb, spec};

pub(crate) fn panel_baseline(checks: &mut Vec<(&'static str, bool, String)>) {
    // 面板底色与预编辑条:(5,60) 在候选区背景上,(5,10) 在拼音条上。
    let mut surface = surfaces::raster_n32_premul((360, 120)).expect("raster surface");
    let metrics = Metrics::standard();
    panel::draw(surface.canvas(), &spec(), &palette(), &metrics, None);
    let expected_bg = rgb(palette().bg);
    let expected_bar = rgb(palette().surface_alt);
    match read_bgr(&mut surface, 5, 60) {
        Some(pixel) => push(
            &mut checks,
            "面板底色",
            pixel == expected_bg,
            format!("{pixel:?} 期望 {expected_bg:?}"),
        ),
        None => push(checks, "面板底色", false, "像素读回失败".into()),
    }
    match read_bgr(&mut surface, 5, 10) {
        Some(pixel) => push(
            &mut checks,
            "预编辑条底色",
            pixel == expected_bar,
            format!("{pixel:?} 期望 {expected_bar:?}"),
        ),
        None => push(checks, "预编辑条底色", false, "像素读回失败".into()),
    }
}

pub(crate) fn panel_geometry(checks: &mut Vec<(&'static str, bool, String)>) {
    let metrics = Metrics::standard();

    // 布局:两个短候选一行;长候选换行;竖排一条一行,行数进窗口高度公式。
    let rows = panel::layout_rows(&spec(), &metrics, None);
    push(
        &mut checks,
        "横排短候选单行",
        rows == 1,
        format!("rows={rows}"),
    );
    let mut long = spec();
    long.items = (0..9)
        .map(|i| Item {
            text: format!("候选{i}是很长的整句词条内容"),
            hint: String::new(),
            selected: false,
        })
        .collect();
    let long_rows = panel::layout_rows(&long, &metrics, None);
    push(
        &mut checks,
        "横排长候选换行",
        long_rows >= 3,
        format!("rows={long_rows}"),
    );
    let mut column = spec();
    column.layout = Layout::Column;
    let column_rows = panel::layout_rows(&column, &metrics, None);
    push(
        &mut checks,
        "竖排一行一条",
        column_rows == column.items.len(),
        format!("rows={column_rows}"),
    );
    let height = panel::panel_height(&spec(), &metrics, None);
    push(
        &mut checks,
        "窗口高度公式",
        // 拼音条 32 + 列表上边距 4 + 一行 30 + 底边距 6。
        (height - (32.0 + 4.0 + 30.0 + 6.0)).abs() < f32::EPSILON,
        format!("height={height}"),
    );

    // 命中测试与绘制共用布局:所指即所画,窗口层不用再写一份命中判定。
    let hit_spec = spec();
    let hit_metrics = Metrics::standard();
    let rects = panel::item_rects(&hit_spec, &hit_metrics, None);
    let first = rects[0];
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        None,
        first.left + 2.0,
        first.top + 2.0,
    );
    push(
        &mut checks,
        "命中候选行",
        hit == panel::Hit::Item(0),
        format!("{hit:?}"),
    );
    let hit = panel::hit_test(&hit_spec, &hit_metrics, None, 5.0, 10.0);
    push(
        &mut checks,
        "命中拼音条空白",
        hit == panel::Hit::Nothing,
        format!("{hit:?}"),
    );
    let hit_width = panel::panel_width(&hit_spec, &hit_metrics, None);
    let (prev, _, next) = panel::pager_rects(hit_width, &hit_metrics);
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        None,
        (prev.left + prev.right) * 0.5,
        (prev.top + prev.bottom) * 0.5,
    );
    push(
        &mut checks,
        "命中上一页箭头",
        hit == panel::Hit::PrevPage,
        format!("{hit:?}"),
    );
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        None,
        (next.left + next.right) * 0.5,
        (next.top + next.bottom) * 0.5,
    );
    push(
        &mut checks,
        "命中下一页箭头",
        hit == panel::Hit::NextPage,
        format!("{hit:?}"),
    );

    // 右键菜单:两行 26px,行间空隙与菜单外都不是菜单项。
    push(
        &mut checks,
        "菜单首行",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 17.0) == Some(0),
        format!("{:?}", panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 17.0)),
    );
    push(
        &mut checks,
        "菜单次行",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 45.0) == Some(1),
        format!("{:?}", panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 45.0)),
    );
    push(
        &mut checks,
        "菜单行间隙与外侧",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 31.0).is_none()
            && panel::menu_hit(0.0, 0.0, &hit_metrics, 200.0, 17.0).is_none(),
        "间隙与 116px 外都落空".into(),
    );

    // 悬停底色:非选中行画 hover 色,绘制与命中同一份矩形。
    let mut hover_surface = surfaces::raster_n32_premul((360, 120)).expect("raster surface");
    let (hw, hh) = (
        panel::panel_width(&hit_spec, &hit_metrics, None),
        panel::panel_height(&hit_spec, &hit_metrics, None),
    );
    panel::draw_chrome(
        hover_surface.canvas(),
        hw,
        hh,
        &hit_spec,
        &palette(),
        &hit_metrics,
        None,
        None,
        panel::Effects::default(),
    );
    panel::draw_items(
        hover_surface.canvas(),
        hw,
        hh,
        &hit_spec,
        &palette(),
        &hit_metrics,
        None,
        panel::ListFx::NEUTRAL,
        Some(1),
        panel::Effects::default(),
    );
    let second = rects[1];
    let expected_hover = rgb(palette().hover);
    match read_bgr(
        &mut hover_surface,
        second.left as i32 + 6,
        second.top as i32 + 6,
    ) {
        Some(pixel) => push(
            &mut checks,
            "悬停底色",
            pixel == expected_hover,
            format!("{pixel:?} 期望 {expected_hover:?}"),
        ),
        None => push(checks, "悬停底色", false, "像素读回失败".into()),
    }

    // 菜单绘制:首行悬停给 hover 底色,次行保持菜单底色;与 menu_hit 同一套行几何。
    let mut menu_surface = surfaces::raster_n32_premul((160, 80)).expect("raster surface");
    panel::draw_menu(
        menu_surface.canvas(),
        0.0,
        0.0,
        &palette(),
        &hit_metrics,
        None,
        Some(0),
        panel::Effects::default(),
    );
    let expected_menu_hover = rgb(palette().hover);
    match read_bgr(&mut menu_surface, 10, 17) {
        Some(pixel) => push(
            &mut checks,
            "菜单悬停底色",
            pixel == expected_menu_hover,
            format!("{pixel:?} 期望 {expected_menu_hover:?}"),
        ),
        None => push(checks, "菜单悬停底色", false, "像素读回失败".into()),
    }
    let expected_menu_bg = rgb(palette().bg);
    match read_bgr(&mut menu_surface, 10, 45) {
        Some(pixel) => push(
            &mut checks,
            "菜单底色",
            pixel == expected_menu_bg,
            format!("{pixel:?} 期望 {expected_menu_bg:?}"),
        ),
        None => push(checks, "菜单底色", false, "像素读回失败".into()),
    }

    // 翻页箭头悬停:位置由 pager_rects 决定,悬停时垫一层描边色底。
    let mut pager_surface = surfaces::raster_n32_premul((360, 120)).expect("raster surface");
    panel::draw_chrome(
        pager_surface.canvas(),
        hw,
        hh,
        &hit_spec,
        &palette(),
        &hit_metrics,
        None,
        Some(panel::Hit::PrevPage),
        panel::Effects::default(),
    );
    let (prev, _, _) = panel::pager_rects(hw, &hit_metrics);
    let expected_pager = rgb(palette().border);
    let (px, py) = (
        ((prev.left + prev.right) * 0.5) as i32,
        ((prev.top + prev.bottom) * 0.5) as i32,
    );
    match read_bgr(&mut pager_surface, px, py) {
        Some(pixel) => push(
            &mut checks,
            "箭头悬停底色",
            pixel == expected_pager,
            format!("{pixel:?} 期望 {expected_pager:?}"),
        ),
        None => push(checks, "箭头悬停底色", false, "像素读回失败".into()),
    }
}
