//! 用户效果契约:SkSL runtime effect 只作用于背景/装饰层。
//!
//! uniform 由引擎按名字打包,作者只写 `main`;字段增减必须同步
//! `EFFECT_API`。效果输出预乘 alpha 颜色,内容层(拼音、候选、序号)
//! 由引擎绘制,契约上不向效果暴露文本。布局按 SkSL 规则由
//! `RuntimeEffect::uniforms()` 自省得出,契约里统一用 `float`,避免
//! `half` 打包差异。
//!
//! 三个槽位各一个文件(`<效果目录>/background.sksl`、`highlight.sksl`、
//! `menu.sksl`):面板底、选中块、菜单底。坏文件只废掉自己的槽位,
//! 其余照常,错误文本留给设置窗与日志。
//!
//! 契约的第一原则是打字反馈:效果在加载期编译,帧内只打包 uniform;
//! 帧耗时超预算就临时摘掉效果(见 [`BudgetGuard`]),冷却后再试。
//!
//! 效果之上还有一层引擎遮罩(见 `panel::scrim_alpha`):底色按不透明度
//! 压在效果上,文字对比度因此有下限,用户效果再花也压不垮可读性。

use std::path::Path;
use std::time::{Duration, Instant};

use skia_safe::{Data, Matrix, RuntimeEffect, Shader, Vector};

/// 效果契约版本;效果文件声明所依赖的版本,高于本值拒绝加载。
pub const EFFECT_API: u32 = 1;

/// 效果槽位:只画装饰层,文字永远由引擎画。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// 面板底。
    Background,
    /// 选中候选的高亮块。
    Highlight,
    /// 右键菜单底。
    Menu,
}

impl Slot {
    /// 全部槽位;加载时按这个顺序找文件。
    pub const ALL: [Slot; 3] = [Slot::Background, Slot::Highlight, Slot::Menu];

    /// 槽位在效果目录里的文件名。
    pub fn file_name(self) -> &'static str {
        match self {
            Slot::Background => "background.sksl",
            Slot::Highlight => "highlight.sksl",
            Slot::Menu => "menu.sksl",
        }
    }

    /// 槽位的中文名;日志与设置窗用。
    pub fn title(self) -> &'static str {
        match self {
            Slot::Background => "面板底",
            Slot::Highlight => "选中块",
            Slot::Menu => "菜单底",
        }
    }

    fn index(self) -> usize {
        match self {
            Slot::Background => 0,
            Slot::Highlight => 1,
            Slot::Menu => 2,
        }
    }
}

/// 背景效果模板:声明契约 uniform,作者只改 `main` 的返回。
pub const BACKGROUND_TEMPLATE: &str = "\
// 掠影背景效果,契约版本 EFFECT_API=1。
// 坐标是本槽位区域内的逻辑坐标(设计像素),返回值是预乘 alpha 颜色。
uniform float2 u_size;   // 本槽位区域的逻辑尺寸
uniform float  u_time;   // 动画时钟(秒);reduce-motion 与组句中冻结为 0
uniform float  u_anim;   // 入场/翻页进度 0..1,静止为 1
uniform float4 u_accent; // 主题色 0..1,RGB + 保留位

half4 main(float2 p) {
    // 在这里实现效果;返回预乘 alpha 颜色。文字层由引擎绘制。
    float edge = min(min(p.x, u_size.x - p.x), min(p.y, u_size.y - p.y));
    float glow = smoothstep(0.0, 24.0, edge);
    float a = (0.55 + 0.35 * glow) * u_anim;
    return half4(u_accent.rgb * a, a);
}
";

/// 选中块效果模板:坐标从选中块左上角起算,画的是选中候选那一块。
pub const HIGHLIGHT_TEMPLATE: &str = "\
// 掠影选中块效果,契约版本 EFFECT_API=1。
// 坐标是本槽位区域内的逻辑坐标(设计像素),返回值是预乘 alpha 颜色。
uniform float2 u_size;   // 本槽位区域的逻辑尺寸
uniform float  u_time;   // 动画时钟(秒)
uniform float  u_anim;   // 入场/翻页进度 0..1
uniform float4 u_accent; // 主题色 0..1,RGB + 保留位

half4 main(float2 p) {
    float pulse = 0.85 + 0.15 * sin(u_time * 2.0);
    float t = p.x / max(u_size.x, 1.0);
    float a = (0.55 + 0.25 * t) * pulse * u_anim;
    return half4(u_accent.rgb * a, a);
}
";

/// 菜单底效果模板:坐标从菜单左上角起算,画的是右键菜单那一块。
pub const MENU_TEMPLATE: &str = "\
// 掠影菜单底效果,契约版本 EFFECT_API=1。
// 坐标是本槽位区域内的逻辑坐标(设计像素),返回值是预乘 alpha 颜色。
uniform float2 u_size;   // 本槽位区域的逻辑尺寸
uniform float  u_time;   // 动画时钟(秒)
uniform float  u_anim;   // 入场/翻页进度 0..1
uniform float4 u_accent; // 主题色 0..1,RGB + 保留位

half4 main(float2 p) {
    float t = p.y / max(u_size.y, 1.0);
    float a = (0.55 + 0.15 * t) * u_anim;
    return half4(u_accent.rgb * a, a);
}
";

/// 某槽位的示例源码;设置窗写文件时用。
pub fn template(slot: Slot) -> &'static str {
    match slot {
        Slot::Background => BACKGROUND_TEMPLATE,
        Slot::Highlight => HIGHLIGHT_TEMPLATE,
        Slot::Menu => MENU_TEMPLATE,
    }
}

/// 每帧发给效果的 uniform 值。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Uniforms {
    /// 本槽位区域的逻辑尺寸(设计像素)。
    pub size: [f32; 2],
    /// 动画时钟(秒);reduce-motion 与组句中冻结为 0。
    pub time: f32,
    /// 入场/翻页动画进度 0..1;静止为 1。
    pub anim: f32,
    /// 主题色 0..1 RGB,第 4 位保留。
    pub accent: [f32; 4],
}

impl Uniforms {
    /// 主题色(0xRRGGBB)转成 0..1 RGB。
    pub fn accent_from_rgb(rgb: u32) -> [f32; 4] {
        [
            ((rgb >> 16) & 0xff) as f32 / 255.0,
            ((rgb >> 8) & 0xff) as f32 / 255.0,
            (rgb & 0xff) as f32 / 255.0,
            0.0,
        ]
    }
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
pub fn make_shader(effect: &RuntimeEffect, uniforms: &Uniforms) -> Option<Shader> {
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

/// 已加载的效果:每个槽位一份编译结果;坏文件只废掉自己的槽位。
#[derive(Default)]
pub struct EffectSet {
    slots: [Option<RuntimeEffect>; 3],
    errors: Vec<String>,
}

impl EffectSet {
    /// 空集合:所有槽位都走素色。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从目录加载:每个槽位读一个 `<槽位>.sksl`;缺文件不算错。
    pub fn load_dir(dir: &Path) -> Self {
        let mut set = Self::default();
        for slot in Slot::ALL {
            let path = dir.join(slot.file_name());
            match std::fs::read_to_string(&path) {
                Ok(source) => {
                    if let Err(e) = set.load_slot(slot, &source) {
                        set.errors.push(e);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => set.errors.push(format!("读 {} 失败: {e}", path.display())),
            }
        }
        set
    }

    /// 编译一个槽位的效果;版本太高或语法错都返回错误文本。
    fn load_slot(&mut self, slot: Slot, source: &str) -> Result<(), String> {
        if let Some(version) = declared_api(source)
            && version > EFFECT_API
        {
            return Err(format!(
                "{} 声明契约版本 {version},当前只支持 {EFFECT_API}",
                slot.title()
            ));
        }
        let effect =
            compile_background(source).map_err(|e| format!("{} 编译失败: {e}", slot.title()))?;
        self.slots[slot.index()] = Some(effect);
        Ok(())
    }

    /// 某槽位有没有可用效果。
    pub fn has(&self, slot: Slot) -> bool {
        self.slots[slot.index()].is_some()
    }

    /// 编译错误文本;设置窗与日志用。
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// 有没有任何可用效果。
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }

    /// 生成某槽位的 shader。
    ///
    /// `origin` 是槽位区域左上角的画布坐标,`unit` 是画布单位与逻辑单位
    /// (设计像素)的比例。Skia 的 local matrix 把 shader 空间映射到设备
    /// 空间,这里取「先缩放后平移」:shader 里量到的坐标从区域左上角
    /// 起算、单位是逻辑单位,契约里的 `u_size` 与之同一套尺子。
    pub fn shader(
        &self,
        slot: Slot,
        uniforms: &Uniforms,
        origin: (f32, f32),
        unit: f32,
    ) -> Option<Shader> {
        let effect = self.slots[slot.index()].as_ref()?;
        let shader = make_shader(effect, uniforms)?;
        let mut local = Matrix::scale((unit, unit));
        local.post_translate(Vector::new(origin.0, origin.1));
        Some(shader.with_local_matrix(&local))
    }
}

/// 读效果源码里的契约版本声明(`// effect-api: N`);没有声明按当前版本。
fn declared_api(source: &str) -> Option<u32> {
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("//") else {
            continue;
        };
        let rest = rest.trim();
        let Some(value) = rest.strip_prefix("effect-api:") else {
            continue;
        };
        return value.trim().parse::<u32>().ok();
    }
    None
}

/// 效果帧预算:单帧效果耗时超阈值就摘掉效果,冷却后再试。
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// 单帧允许的毫秒数。
    pub limit_ms: f32,
    /// 连续超预算几次就摘掉效果。
    pub max_streak: u32,
    /// 摘掉后多久再试(毫秒)。
    pub cooldown_ms: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            limit_ms: 4.0,
            max_streak: 2,
            cooldown_ms: 2000,
        }
    }
}

/// 预算状态机:喂进每帧耗时,决定这一帧要不要上效果。
#[derive(Debug)]
pub struct BudgetGuard {
    budget: Budget,
    /// 连续超预算次数。
    streak: u32,
    /// 摘掉效果到什么时候为止。
    disabled_until: Option<Instant>,
    /// 刚恢复效果,这一帧只预热不计入预算。
    warming: bool,
}

impl BudgetGuard {
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            streak: 0,
            disabled_until: None,
            warming: false,
        }
    }

    /// 这一帧能不能上效果;刚从冷却里恢复时先预热一帧。
    pub fn allows(&mut self, now: Instant) -> bool {
        match self.disabled_until {
            Some(until) if now < until => false,
            Some(_) => {
                self.disabled_until = None;
                self.streak = 0;
                self.warming = true;
                true
            }
            None => true,
        }
    }

    /// 喂一帧耗时;连续超预算就摘掉效果并进入冷却。
    pub fn observe(&mut self, elapsed: Duration, now: Instant) {
        if self.warming {
            self.warming = false;
            return;
        }
        if elapsed.as_secs_f32() * 1000.0 > self.budget.limit_ms {
            self.streak += 1;
            if self.streak >= self.budget.max_streak {
                self.disabled_until = Some(now + Duration::from_millis(self.budget.cooldown_ms));
                self.streak = 0;
            }
        } else {
            self.streak = 0;
        }
    }
}
