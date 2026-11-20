//! 候选面板布局 E2E:固定条(自适应条数)、展开网格、窗口尺寸公式与命中测试。
//!
//! 像素级断言在 Windows 侧的 D2D 自检(`LUEYING_D2D_SELFTEST`,WARP 离屏)
//! 覆盖;Linux 这里只验布局数学,文字宽度走 `Estimate` 估算,两个平台同一
//! 套几何公式。

use lueying_render::panel::{self, Estimate, GridItem, Item, Layout, Measure, Metrics, STRIP_W};

use crate::harness::{push, spec};

pub(crate) fn panel_geometry(checks: &mut Vec<(&'static str, bool, String)>) {
    let metrics = Metrics::standard();
    let estimate = Estimate;

    // 条容量自适应:单字候选窄,一条里能排七八条;长句整词宽,只放得下一
    // 条。数字键最多选到 9,条数上限也是 9。单字候选就是一个字,别拿两字
    // 词当代理(两字词实测六条,与微信输入法一致)。
    let chars = ["是", "时", "事", "实", "式", "视", "试", "市", "示"]
        .iter()
        .map(|t| estimate.width(t, metrics.text));
    let char_fit = panel::strip_capacity(chars, &metrics);
    push(
        checks,
        "单字候选排七八条",
        (7..=9).contains(&char_fit),
        format!("char_fit={char_fit}"),
    );
    let long_text = "这一条候选是很长的整句词条内容超宽";
    let longs = (0..3).map(|i| estimate.width(&format!("{long_text}{i}"), metrics.text));
    let long_fit = panel::strip_capacity(longs, &metrics);
    push(
        checks,
        "长句候选整条占满",
        long_fit == 1,
        format!("long_fit={long_fit}"),
    );

    // 翻页只换当前页的候选区间;页号越界收进最后一页,末页不足一页也截断。
    let page_ok = panel::page_bounds(0, 5, 12) == (0..5)
        && panel::page_bounds(1, 5, 12) == (5..10)
        && panel::page_bounds(2, 5, 12) == (10..12)
        && panel::page_bounds(9, 5, 12) == (10..12)
        && panel::page_bounds(0, 5, 0) == (0..0);
    push(
        checks,
        "页区间跟随页号",
        page_ok,
        format!(
            "第 2 页 {:?},末页 {:?}",
            panel::page_bounds(1, 5, 12),
            panel::page_bounds(2, 5, 12)
        ),
    );

    // 棋盘是连续滚动窗口:底行按下滚一行,末格按右滚一行,顶行按上滚回去。
    // 滚动始终按整行推进,候选在屏内的列不变。
    let down = panel::grid_step(20, 0, 50, 0, 1);
    let down_last = panel::grid_step(23, 0, 50, 1, 0);
    let up_row = panel::grid_step(24, 4, 50, 0, -1);
    let up_top = panel::grid_step(4, 4, 50, 0, -1);
    let move_ok =
        down == panel::GridStep {
            highlight: 24,
            scroll: 4,
        } && down_last
            == panel::GridStep {
                highlight: 24,
                scroll: 4,
            }
            && up_row
                == panel::GridStep {
                    highlight: 20,
                    scroll: 4,
                }
            && up_top
                == panel::GridStep {
                    highlight: 0,
                    scroll: 0,
                }
            && panel::grid_step(0, 0, 50, 1, 0)
                == panel::GridStep {
                    highlight: 1,
                    scroll: 0,
                }
            && panel::grid_step(2, 0, 50, 1, 0)
                == panel::GridStep {
                    highlight: 3,
                    scroll: 0,
                }
            && panel::grid_step(3, 0, 50, -1, 0)
                == panel::GridStep {
                    highlight: 2,
                    scroll: 0,
                }
            && panel::grid_step(6, 0, 50, 0, -1)
                == panel::GridStep {
                    highlight: 2,
                    scroll: 0,
                };
    push(
        checks,
        "棋盘方向键整行滚动",
        move_ok,
        format!("底行↓ {down:?},末格→ {down_last:?},上一行↑ {up_row:?}"),
    );
    // 第六行第四列按右:下一行第一个候选从底行左端进入,旧词保持列不变。
    let right_at_end = panel::grid_step(23, 0, 50, 1, 0);
    let old_col = (19usize.wrapping_sub(right_at_end.scroll)) % panel::GRID_COLS;
    push(
        checks,
        "末格右移不改候选列",
        right_at_end.highlight == 24 && right_at_end.scroll == 4 && old_col == 3,
        format!("right={right_at_end:?}, 原第四列词新列={old_col}"),
    );
    let bar = panel::scrollbar_rects(
        panel::Scroll {
            offset: 0,
            len: 50,
            cap: panel::GRID_MAX,
        },
        480.0,
        210.0,
        &metrics,
        false,
    );
    let hot = panel::scrollbar_rects(
        panel::Scroll {
            offset: 0,
            len: 50,
            cap: panel::GRID_MAX,
        },
        480.0,
        210.0,
        &metrics,
        true,
    );
    let no_bar = panel::scrollbar_rects(
        panel::Scroll {
            offset: 0,
            len: 20,
            cap: panel::GRID_MAX,
        },
        480.0,
        210.0,
        &metrics,
        false,
    );
    push(
        checks,
        "棋盘滚动条只在超一屏时出现",
        bar.is_some() && no_bar.is_none(),
        format!("50 条 {bar:?},20 条 {no_bar:?}"),
    );
    push(
        checks,
        "滚动条悬停加宽",
        hot.is_some_and(|(track, thumb)| {
            let (idle, _) = bar.unwrap();
            track.width() > idle.width() && (thumb.width() - track.width()).abs() < f32::EPSILON
        }),
        format!("idle={bar:?}, hover={hot:?}"),
    );

    // 固定条:宽度与行数恒定,打字期间窗口不形变。
    let rows = panel::layout_rows(&spec(), &metrics, &estimate);
    push(checks, "固定条恒一行", rows == 1, format!("rows={rows}"));
    let width = panel::panel_width(&spec(), &metrics, &estimate);
    push(
        checks,
        "固定条宽度恒定",
        (width - STRIP_W).abs() < f32::EPSILON,
        format!("width={width}"),
    );
    let mut long = spec();
    long.items = (0..3)
        .map(|i| Item {
            text: format!("{long_text}{i}"),
            hint: String::new(),
            selected: false,
        })
        .collect();
    let long_rows = panel::layout_rows(&long, &metrics, &estimate);
    let long_width = panel::panel_width(&long, &metrics, &estimate);
    push(
        checks,
        "固定条装不下长句也不变形",
        long_rows == 1 && (long_width - STRIP_W).abs() < f32::EPSILON,
        format!("rows={long_rows} width={long_width}"),
    );

    // 条内截断只发生在单条候选超过整条宽时:省略号,且量出来确实装进条宽。
    let mut huge = spec();
    huge.items = vec![Item {
        text: long_text.repeat(2),
        hint: String::new(),
        selected: false,
    }];
    let huge_rect = panel::item_rects(&huge, &metrics, &estimate)[0];
    let text_x = huge_rect.left + panel::TEXT_X;
    let budget = width - panel::PAD_X - text_x - panel::ITEM_PAD;
    let shown = panel::ellipsize(&huge.items[0].text, budget, metrics.text, &estimate);
    push(
        checks,
        "超宽整条才截断",
        shown != huge.items[0].text
            && shown.ends_with('…')
            && estimate.width(&shown, metrics.text) <= budget,
        format!(
            "原文 {} 字 → {shown} 字",
            huge.items[0].text.chars().count()
        ),
    );
    let height = panel::panel_height(&spec(), &metrics, &estimate);
    push(
        checks,
        "固定条高度公式",
        // 上边距 4 + 一行 30 + 底边距 6。
        (height - (4.0 + 30.0 + 6.0)).abs() < f32::EPSILON,
        format!("height={height}"),
    );

    // 展开棋盘:等宽等高的 4 列单元,顶右一格留给收起钮;窗口宽度恒定,
    // 不随候选长短伸缩。
    let mut expanded = spec();
    expanded.expand = true;
    expanded.grid = (0..3)
        .map(|i| GridItem {
            text: format!("后续候选{i}"),
            index: 2 + i,
            selected: false,
        })
        .collect();
    let expand_rows = panel::layout_rows(&expanded, &metrics, &estimate);
    push(
        checks,
        "展开棋盘 4 列成行",
        expand_rows == 2,
        format!("rows={expand_rows}"),
    );
    let expand_width = panel::panel_width(&expanded, &metrics, &estimate);
    push(
        checks,
        "展开态宽度恒定",
        (expand_width - STRIP_W).abs() < f32::EPSILON,
        format!("width={expand_width}"),
    );
    let cells = panel::grid_cells(&expanded, &metrics, &estimate);
    push(
        checks,
        "棋盘等宽等高",
        cells.iter().all(|rect| {
            (rect.width() - cells[0].width()).abs() < 0.01
                && (rect.height() - cells[0].height()).abs() < 0.01
        }),
        format!("{} 格", cells.len()),
    );
    let first_row = cells
        .iter()
        .filter(|rect| (rect.top - cells[0].top).abs() < 0.01)
        .count();
    push(
        checks,
        "首行四格满",
        first_row == panel::GRID_COLS,
        format!("首行 {first_row} 格"),
    );
    let grid_rects = panel::grid_rects(&expanded, &metrics, &estimate);
    push(
        checks,
        "展开棋盘一条一格",
        grid_rects.len() == expanded.grid.len(),
        format!("{} 格", grid_rects.len()),
    );
    let cell_hit = panel::hit_test(
        &expanded,
        &metrics,
        &estimate,
        grid_rects[0].center_x(),
        grid_rects[0].center_y(),
    );
    push(
        checks,
        "网格命中带绝对序号",
        cell_hit == panel::Hit::GridItem(2),
        format!("{cell_hit:?}"),
    );
    let chevron = panel::chevron_rect(&expanded, &metrics, &estimate);
    let chevron_hit = chevron.map(|rect| {
        panel::hit_test(
            &expanded,
            &metrics,
            &estimate,
            rect.center_x(),
            rect.center_y(),
        )
    });
    push(
        checks,
        "收起钮在右侧控制轨顶部",
        chevron_hit == Some(panel::Hit::Expand)
            && chevron.is_some_and(|rect| {
                rect.top >= cells[0].top - 0.01
                    && rect.bottom <= cells[0].bottom + 0.01
                    && rect.left >= cells[panel::GRID_COLS - 1].right - 0.01
            }),
        format!("{chevron_hit:?}, chevron={chevron:?}"),
    );

    // 命中测试与绘制共用布局:所指即所画,窗口层不用再写一份命中判定。
    let hit_spec = spec();
    let rects = panel::item_rects(&hit_spec, &metrics, &estimate);
    let first = rects[0];
    let hit = panel::hit_test(
        &hit_spec,
        &metrics,
        &estimate,
        first.left + 2.0,
        first.top + 2.0,
    );
    push(
        checks,
        "命中候选槽",
        hit == panel::Hit::Item(0),
        format!("{hit:?}"),
    );
    let second_hit = panel::hit_test(
        &hit_spec,
        &metrics,
        &estimate,
        rects[1].center_x(),
        rects[1].center_y(),
    );
    push(
        checks,
        "命中第二条",
        second_hit == panel::Hit::Item(1),
        format!("{second_hit:?}"),
    );
    let chev = panel::chevron_rect(&hit_spec, &metrics, &estimate).expect("条尾按钮");
    let hit = panel::hit_test(
        &hit_spec,
        &metrics,
        &estimate,
        chev.center_x(),
        chev.center_y(),
    );
    push(
        checks,
        "命中条尾展开按钮",
        hit == panel::Hit::Expand,
        format!("{hit:?}"),
    );
    // 条内左侧留白:第一个槽从 PAD_X 开始,采样点必须在它左边。
    let hit = panel::hit_test(&hit_spec, &metrics, &estimate, 2.0, 10.0);
    push(
        checks,
        "命中条内空白",
        hit == panel::Hit::Nothing,
        format!("{hit:?}"),
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

    // 右键菜单:两行 26px,行间空隙与菜单外都不是菜单项。
    push(
        checks,
        "菜单首行",
        panel::menu_hit(0.0, 0.0, &metrics, 10.0, 17.0) == Some(0),
        format!("{:?}", panel::menu_hit(0.0, 0.0, &metrics, 10.0, 17.0)),
    );
    push(
        checks,
        "菜单次行",
        panel::menu_hit(0.0, 0.0, &metrics, 10.0, 45.0) == Some(1),
        format!("{:?}", panel::menu_hit(0.0, 0.0, &metrics, 10.0, 45.0)),
    );
    push(
        checks,
        "菜单行间隙与外侧",
        panel::menu_hit(0.0, 0.0, &metrics, 10.0, 31.0).is_none()
            && panel::menu_hit(0.0, 0.0, &metrics, 200.0, 17.0).is_none(),
        "间隙与 116px 外都落空".into(),
    );
}
