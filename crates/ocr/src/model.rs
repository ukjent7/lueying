//! ONNX 模型加载:解析、优化、编译成可复用计划。
//!
//! 模型输入带动态尺寸(截图大小、文字行宽度都不固定),这里不钉死形状;
//! tract 在每次 `run` 时从实际输入张量解析符号,同一份计划可以反复用。

use std::path::Path;
use std::sync::Arc;

use tract_onnx::prelude::*;

pub(crate) type Plan = Arc<TypedRunnableModel>;

pub(crate) fn load(path: &Path) -> Result<Plan, String> {
    if !path.is_file() {
        return Err(format!("模型缺失:{}", path.display()));
    }
    let model = tract_onnx::onnx()
        .model_for_path(path)
        .map_err(|e| format!("解析 {} 失败: {e}", path.display()))?
        .into_optimized()
        .map_err(|e| format!("优化 {} 失败: {e}", path.display()))?;
    model
        .into_runnable()
        .map_err(|e| format!("编译 {} 失败: {e}", path.display()))
}
