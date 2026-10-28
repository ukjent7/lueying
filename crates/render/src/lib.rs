//! 候选窗 Skia 渲染核心:平台无关,吃数据出帧。
//!
//! GPUI 路径仍是默认;本 crate 先在无头表面出帧,供 E2E 断言与后续
//! Skia 窗口共用同一套绘制。窗口、动画与命中判定不在这里;文字截断
//! 先用矩形裁剪,省略号留到窗口接入后视觉核对时补。

pub mod effect;
pub mod panel;

pub use panel::{draw, item_width, layout_rows, panel_height, panel_width, Item, Layout, Metrics, Palette, PanelSpec, PreeditBar};
