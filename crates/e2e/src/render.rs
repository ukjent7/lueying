//! 渲染层 E2E:面板布局数学与设置窗绘制(像素级候选断言在 Windows D2D 自检)。

mod panel;
mod ui;

use crate::harness::{self, Checks};
use crate::render::panel::panel_geometry;
use crate::render::ui::ui_checks;

/// 运行全部渲染检查,把结果写进报告并返回失败数。
pub fn run(report: &mut String) -> usize {
    let mut checks = Checks::new();

    panel_geometry(&mut checks);
    ui_checks(&mut checks);

    harness::write(report, "渲染核心", checks)
}
