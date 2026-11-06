//! 输入法表面渲染核心:布局、命中与绘制指令,平台无关。
//!
//! 候选窗的行几何(`item_rects`/`pager_rects`)与窗口尺寸公式在这里;
//! 文字测宽走 [`panel::Measure`](Windows 侧 DirectWrite 实量,E2E 走
//! [`panel::Estimate`])。徽标、命令面板与选择面板只产出绘制指令,
//! 由平台侧的 Direct2D 画布执行。

pub mod actor;
pub mod animation;
pub mod badge;
pub mod command;
pub mod paint;
pub mod panel;
pub mod picker;
pub mod spring;
pub mod toolbar;

pub use paint::{Canvas, Color, Point};
pub use panel::{
    Estimate, Hit, Item, Layout, ListFx, Measure, Metrics, Palette, PanelSpec, PreeditBar, Rect,
    Surface, hit_test, item_rects, item_width, layout_rows, menu_height, menu_hit, pager_rects,
    panel_height, panel_width,
};
pub use spring::Spring;
