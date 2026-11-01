//! 候选窗渲染核心:布局与命中的平台无关部分,吃数据出几何。
//!
//! 候选窗的行几何(`item_rects`/`pager_rects`)与窗口尺寸公式在这里;
//! 文字测宽走 [`panel::Measure`](Windows 侧 DirectWrite 实量,E2E 走
//! [`panel::Estimate`])。窗口、动效与像素绘制都在 server 的 D2D 管线
//! (`ui_d2d`);命中判定与绘制共用同一份行几何,窗口层只翻译命中结果。

pub mod badge;
pub mod command;
pub mod paint;
pub mod panel;
pub mod picker;
pub mod ui;

pub use panel::{
    Estimate, Hit, Item, Layout, ListFx, Measure, Metrics, Palette, PanelSpec, PreeditBar, Rect,
    hit_test, item_rects, item_width, layout_rows, menu_height, menu_hit, pager_rects,
    panel_height, panel_width,
};
