//! ONNX 模型加载:解析、优化、编译成可复用计划。
//!
//! det 的多尺度上采样在符号尺寸下没法在 tract 里统一,输入端钉死成
//! 固定形状;rec 沿宽度动态,宽度由文本框决定。

use std::path::Path;
use std::sync::Arc;

use tract_onnx::prelude::*;

pub(crate) type Plan = Arc<TypedRunnableModel>;

/// `fixed_shape` 为 `None` 时保留模型自带的动态尺寸。
pub(crate) fn load(path: &Path, fixed_shape: Option<&[usize]>) -> Result<Plan, String> {
    if !path.is_file() {
        return Err(format!("模型缺失:{}", path.display()));
    }
    // tract 的 Display 只给最后一句,这里用 Debug 带上原因链。
    let mut model = tract_onnx::onnx()
        // 模型自带的中间张量形状来自 Paddle2ONNX 导出时的推断,Resize
        // 这类节点的声明和实际不一致;交给 tract 自己推。
        .with_ignore_value_info(true)
        .model_for_path(path)
        .map_err(|e| format!("解析 {} 失败: {e:?}", path.display()))?;
    if let Some(shape) = fixed_shape {
        model = model
            .with_input_fact(
                0,
                InferenceFact::dt_shape(f32::datum_type(), shape.to_vec()),
            )
            .map_err(|e| format!("固定 {} 输入形状失败: {e:?}", path.display()))?;
    }
    let model = model
        .into_optimized()
        .map_err(|e| format!("优化 {} 失败: {e:?}", path.display()))?;
    model
        .into_runnable()
        .map_err(|e| format!("编译 {} 失败: {e:?}", path.display()))
}
