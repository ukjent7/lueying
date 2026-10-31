//! Skia 渲染核心 E2E:光栅出帧、SkSL 效果契约与坏效果回退。
//!
//! 先列失败方式:
//! 1. 面板底色/预编辑条画错颜色或圆角把角上像素染成底色。
//! 2. SkSL 模板编译失败,runtime effect 在光栅后端不可用。
//! 3. uniform 打包错位:效果读到的 u_accent/u_size 不是契约值。
//! 4. 坏 SkSL 没有返回错误,或错误被静默吞掉。
//! 5. 换行/竖排行数与 server 的窗口高度公式不一致。
//! 6. 用户效果盖过文字:遮罩没把对比度守住。

use crate::harness::Checks;
use crate::render_effects::effect_checks;
use crate::render_panel::{panel_baseline, panel_geometry};
use crate::render_ui::ui_checks;

/// 运行全部渲染检查,把结果写进报告并返回失败数。
pub fn run(report: &mut String) -> usize {
    let mut checks = Checks::new();

    panel_baseline(&mut checks);
    effect_checks(&mut checks);
    panel_geometry(&mut checks);
    ui_checks(&mut checks);

    harness::write(report, "Skia 渲染核心", checks)
}
