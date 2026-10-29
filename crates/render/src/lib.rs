//! 候选窗 Skia 渲染核心:平台无关,吃数据出帧。
//!
//! GPUI 路径仍是默认;本 crate 先在无头表面出帧,供 E2E 断言与后续
//! Skia 窗口共用同一套绘制。窗口与动效不在这里;命中判定与绘制共用
//! 同一份行几何(`item_rects`/`pager_rects`),窗口层只翻译命中结果。
//! 文字截断先用矩形裁剪,省略号留到窗口接入后视觉核对时补。

pub mod effect;
pub mod panel;

pub use panel::{
    draw, draw_chrome, draw_items, draw_menu, hit_test, item_rects, item_width, layout_rows,
    menu_height, menu_hit, pager_rects, panel_height, panel_width, Hit, Item, Layout, ListFx,
    Metrics, Palette, PanelSpec, PreeditBar,
};
