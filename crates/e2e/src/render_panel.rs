//! 候选面板布局 E2E:行数、窗口尺寸公式与命中测试。
//!
//! 像素级断言在 Windows 侧的 D2D 自检(`LUEYING_D2D_SELFTEST`,WARP 离屏)
//! 覆盖;Linux 这里只验布局数学,文字宽度走 `Estimate` 估算,两个平台同一
//! 套几何公式。

use lueying_render::panel::{self, Estimate, Item, Layout, Metrics};

use crate::harness::{push, spec};

pub(crate) fn panel_geometry(checks: &mut Vec<(&'static str, bool, String)>) {
    let metrics = Metrics::standard();
    let estimate = Estimate;

    // 布局:两个短候选一行;长候选换行;竖排一条一行,行数进窗口高度公式。
    let rows = panel::layout_rows(&spec(), &metrics, &estimate);
    push(checks, "横排短候选单行", rows == 1, format!("rows={rows}"));
    let mut long = spec();
    long.items = (0..9)
        .map(|i| Item {
            text: format!("候选{i}是很长的整句词条内容"),
            hint: String::new(),
            selected: false,
        })
        .collect();
    let long_rows = panel::layout_rows(&long, &metrics, &estimate);
    push(
        checks,
        "横排长候选换行",
        long_rows >= 3,
        format!("rows={long_rows}"),
    );
    let mut column = spec();
    column.layout = Layout::Column;
    let column_rows = panel::layout_rows(&column, &metrics, &estimate);
    push(
        checks,
        "竖排一行一条",
        column_rows == column.items.len(),
        format!("rows={column_rows}"),
    );
    let height = panel::panel_height(&spec(), &metrics, &estimate);
    push(
        checks,
        "窗口高度公式",
        // 拼音条 32 + 列表上边距 4 + 一行 30 + 底边距 6。
        (height - (32.0 + 4.0 + 30.0 + 6.0)).abs() < f32::EPSILON,
        format!("height={height}"),
    );

    // 命中测试与绘制共用布局:所指即所画,窗口层不用再写一份命中判定。
    let hit_spec = spec();
    let hit_metrics = Metrics::standard();
    let rects = panel::item_rects(&hit_spec, &hit_metrics, &estimate);
    let first = rects[0];
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        &estimate,
        first.left + 2.0,
        first.top + 2.0,
    );
    push(
        checks,
        "命中候选行",
        hit == panel::Hit::Item(0),
        format!("{hit:?}"),
    );
    let hit = panel::hit_test(&hit_spec, &hit_metrics, &estimate, 5.0, 10.0);
    push(
        checks,
        "命中拼音条空白",
        hit == panel::Hit::Nothing,
        format!("{hit:?}"),
    );
    let hit_width = panel::panel_width(&hit_spec, &hit_metrics, &estimate);
    let (prev, _, next) = panel::pager_rects(hit_width, &hit_metrics);
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        &estimate,
        (prev.left + prev.right) * 0.5,
        (prev.top + prev.bottom) * 0.5,
    );
    push(
        checks,
        "命中上一页箭头",
        hit == panel::Hit::PrevPage,
        format!("{hit:?}"),
    );
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        &estimate,
        (next.left + next.right) * 0.5,
        (next.top + next.bottom) * 0.5,
    );
    push(
        checks,
        "命中下一页箭头",
        hit == panel::Hit::NextPage,
        format!("{hit:?}"),
    );

    // 右键菜单:两行 26px,行间空隙与菜单外都不是菜单项。
    push(
        checks,
        "菜单首行",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 17.0) == Some(0),
        format!("{:?}", panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 17.0)),
    );
    push(
        checks,
        "菜单次行",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 45.0) == Some(1),
        format!("{:?}", panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 45.0)),
    );
    push(
        checks,
        "菜单行间隙与外侧",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 31.0).is_none()
            && panel::menu_hit(0.0, 0.0, &hit_metrics, 200.0, 17.0).is_none(),
        "间隙与 116px 外都落空".into(),
    );
}
