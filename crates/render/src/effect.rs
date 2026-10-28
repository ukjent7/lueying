//! 用户效果契约:SkSL runtime effect 只作用于背景/装饰层。
//!
//! uniform 由引擎按名字打包,作者只写 `main`;字段增减必须同步
//! `EFFECT_API`。效果输出预乘 alpha 颜色,内容层(拼音、候选、序号)
//! 由引擎绘制,契约上不向效果暴露文本。布局按 SkSL 规则由
//! `RuntimeEffect::uniforms()` 自省得出,契约里统一用 `float`,避免
//! `half` 打包差异。

use skia_safe::{effects::RuntimeEffect, Data, Shader};

/// 效果契约版本;效果文件声明所依赖的版本,高于本值拒绝加载。
pub const EFFECT_API: u32 = 1;

/// 背景效果模板:声明契约 uniform,作者只改 `main` 的返回。
pub const BACKGROUND_TEMPLATE: &str = "\
// 掠影背景效果,契约版本 EFFECT_API=1。
uniform float2 u_size;   // 面板逻辑尺寸
uniform float  u_time;   // 动画时钟(秒);reduce-motion 与组句中冻结为 0
uniform float  u_anim;   // 入场/翻页进度 0..1,静止为 1
uniform float4 u_accent; // 主题色 0..1,RGB + 保留位

half4 main(float2 p) {
    // 在这里实现效果;返回预乘 alpha 颜色。文字层由引擎绘制。
    float edge = min(min(p.x, u_size.x - p.x), min(p.y, u_size.y - p.y));
    float glow = smoothstep(0.0, 24.0, edge);
    return half4(u_accent.rgb * (0.10 + 0.04 * glow), 1.0);
}
";

/// 每帧发给效果的 uniform 值。
#[derive(Clone, Copy, Debug)]
pub struct Uniforms {
    /// 面板逻辑尺寸。
    pub size: [f32; 2],
    /// 动画时钟(秒);reduce-motion 与组句中冻结为 0。
    pub time: f32,
    /// 入场/翻页动画进度 0..1;静止为 1。
    pub anim: f32,
    /// 主题色 0..1 RGB,第 4 位保留。
    pub accent: [f32; 4],
}

/// 编译背景效果;SkSL 语法错或禁用作 shader 都返回错误文本。
pub fn compile_background(source: &str) -> Result<RuntimeEffect, String> {
    let effect = RuntimeEffect::make_for_shader(source, None)?;
    if !effect.allow_shader() {
        return Err("该效果不允许用作 shader".into());
    }
    Ok(effect)
}

/// 按 uniform 自省打包数值并生成 shader;契约外的 uniform 保持 0。
pub fn background_shader(effect: &RuntimeEffect, uniforms: &Uniforms) -> Option<Shader> {
    let mut bytes = vec![0u8; effect.uniform_size()];
    for uniform in effect.uniforms() {
        let values: &[f32] = match uniform.name() {
            "u_size" => &uniforms.size,
            "u_time" => std::slice::from_ref(&uniforms.time),
            "u_anim" => std::slice::from_ref(&uniforms.anim),
            "u_accent" => &uniforms.accent,
            _ => continue,
        };
        let offset = uniform.offset();
        for (index, value) in values.iter().enumerate() {
            let at = offset + index * 4;
            if at + 4 <= bytes.len() {
                bytes[at..at + 4].copy_from_slice(&value.to_ne_bytes());
            }
        }
    }
    effect.make_shader(Data::new_copy(&bytes), &[], None)
}
