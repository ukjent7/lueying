//! 候选窗的脚本外观:场景转换、补丁落到主题色与 SkSL 效果。
//!
//! 脚本线程本身在 [`crate::ext_host`];这里只做窗口线程这一侧:把面板状态
//! 转成不含候选文字的场景,把补丁编译成 Skia 对象。补丁只在内容变化时
//! 重绘一帧;坏源码只废掉自己的槽位,高对比度下效果整个关掉。

use lueying_extjs::{EffectSlot, EffectsPatch, SceneIn, ScenePatch};
use lueying_protocol::SessionView;
use lueying_render::effect::{EffectSet, Slot, Uniforms};
use lueying_server::theme;
use skia_safe::Shader;

use crate::imp::State;
use crate::ui_place;

/// 面板状态转场景;只含几何与主题,没有候选文字与宿主名。
pub(super) fn scene(
    state: &State,
    view: &SessionView,
    private: bool,
    reduce_motion: bool,
    time: f32,
    anim: f32,
) -> SceneIn {
    let (_, _, accent) = ui_place::ui_options(state);
    let appearance = theme::Appearance::of_system();
    let colors = theme::colors_with(appearance, accent);
    let page_size = view.page_size.max(1);
    let start = view.page * page_size;
    let candidates = view.candidates.len().saturating_sub(start).min(page_size);
    let selected = if candidates == 0 {
        0
    } else {
        view.highlight.saturating_sub(start).min(candidates - 1)
    };
    SceneIn {
        composing: true,
        page: (view.page + 1) as u32,
        pages: view.candidates.len().div_ceil(page_size).max(1) as u32,
        candidates: candidates as u32,
        selected: selected as u32,
        accent: colors.accent,
        dark: matches!(appearance, theme::Appearance::Dark),
        high_contrast: matches!(appearance, theme::Appearance::HighContrast),
        reduce_motion,
        time,
        anim,
        private,
        ..SceneIn::default()
    }
}

/// 已应用的脚本补丁:主题色进调色板,效果编译成 Skia 对象。
pub(super) struct ScriptFx {
    patch: ScenePatch,
    effects: Option<EffectSet>,
}

impl ScriptFx {
    /// 编译一份新补丁;效果源码没变就复用上一份已编译的对象。
    pub(super) fn update(previous: Option<Self>, patch: ScenePatch) -> Self {
        let effects = match previous {
            Some(mut old) if old.patch.effects == patch.effects => old.effects.take(),
            _ => patch.effects.as_ref().map(compile),
        };
        Self { patch, effects }
    }

    /// 补丁里的主题色;没给就沿用设置。
    pub(super) fn accent(&self) -> Option<u32> {
        self.patch.accent
    }

    /// 生成脚本效果的 shader;没给或编译失败都返回 `None`。
    pub(super) fn shader(
        &self,
        slot: Slot,
        uniforms: &Uniforms,
        origin: (f32, f32),
        unit: f32,
    ) -> Option<Shader> {
        self.effects.as_ref()?.shader(slot, uniforms, origin, unit)
    }

    /// 与另一份补丁是否完全相同;相同就不重绘。
    pub(super) fn same(&self, other: &ScenePatch) -> bool {
        &self.patch == other
    }
}

/// 编译补丁里的效果源码;坏源码只废掉自己的槽位,错误写日志。
fn compile(sources: &EffectsPatch) -> EffectSet {
    let set = EffectSet::from_sources(sources.iter().map(|(slot, source)| (slot_of(slot), source)));
    for error in set.errors() {
        println!("扩展效果:{error}");
    }
    set
}

/// 扩展槽位转渲染槽位。
fn slot_of(slot: EffectSlot) -> Slot {
    match slot {
        EffectSlot::Background => Slot::Background,
        EffectSlot::Highlight => Slot::Highlight,
        EffectSlot::Menu => Slot::Menu,
    }
}
