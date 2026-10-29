//! 候选窗 Skia 渲染核心:平台无关,吃数据出帧。
//!
//! GPUI 路径仍是默认;本 crate 先在无头表面出帧,供 E2E 断言与后续
//! Skia 窗口共用同一套绘制。窗口与动效不在这里;命中判定与绘制共用
//! 同一份行几何(`item_rects`/`pager_rects`),窗口层只翻译命中结果。
//! 文字截断先用矩形裁剪,省略号留到窗口接入后视觉核对时补。
//!
//! 用户效果(`effect`模块)按三个槽位加载 SkSL:面板底、选中块、菜单底。
//! 效果只画装饰层,文字永远由引擎绘制;加载期编译,帧内只打包 uniform。
//! 效果之上压一层引擎遮罩(`scrim_alpha`),文字对比度因此有下限。

pub mod effect;
pub mod panel;

pub use panel::{
    CONTRAST_TEXT, CONTRAST_WEAK, Effects, Hit, Item, Layout, ListFx, Metrics, Palette, PanelSpec,
    PreeditBar, draw, draw_chrome, draw_items, draw_menu, hit_test, item_rects, item_width,
    layout_rows, menu_height, menu_hit, pager_rects, panel_height, panel_width, scrim_alpha,
};
