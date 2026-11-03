//! 掠影官方内置扩展:自定义短语、下一词联想,以及快捷输入、表情、
//! 英文混输与简繁转换。双拼与形码在 `extensions/input-schemes`。
//!
//! 这些能力都通过 [`lueying_ext`] 的公开契约接入,核心引擎只保留全拼、
//! 候选生成与学习。宿主按配置选择方案并注册模块。

pub mod emoji;
pub mod english;
pub mod phrases;
pub mod predict;
pub mod quick;
pub mod script;
mod symbols;

use std::sync::Arc;

pub use emoji::EmojiModule;
pub use english::EnglishModule;
pub use lueying_ext::Script;
pub use phrases::CustomPhrases;
pub use lueying_input_schemes::{
    AuxCode, AuxLookup, AuxScheme, CodeEntry, CodeScheme, CodeTable, SchemeId, Shuangpin, scheme,
};
pub use predict::Prediction;
pub use quick::QuickModule;
pub use script::{ScriptModule, convert};

use lueying_ext::{Module, Modules};

/// 组合第一批内置模块。自定义短语按宿主给出的文件创建;联想默认启用。
pub fn registry(phrases: Option<Arc<CustomPhrases>>) -> Arc<Modules> {
    registry_with_prediction(phrases, Arc::new(Prediction::new()))
}

/// 与 [`registry`] 相同,但由宿主持有联想开关,设置变更时无需重建注册表。
pub fn registry_with_prediction(
    phrases: Option<Arc<CustomPhrases>>,
    prediction: Arc<Prediction>,
) -> Arc<Modules> {
    Arc::new(registry_inner(phrases, prediction))
}

/// 只含快捷输入、表情、英文混输与简繁转换的内置模块,按生产注册顺序组合。
/// 裸引擎构造(基准、主题回归)用它补齐与产品一致的候选装饰。
pub fn builtins(script: Script) -> Modules {
    complete_builtins(Modules::new(), &Arc::new(ScriptModule::new(script)))
}

/// 把快捷输入插到宿主模块前、表情/英文/简繁放到后,复现核心时代的阶段
/// 顺序:快捷候选先于宿主功能生成,装饰在宿主功能之后。简繁用宿主给出
/// 的句柄,设置变更时直接切换,无需重建注册表。
pub fn complete_builtins(modules: Modules, script: &Arc<ScriptModule>) -> Modules {
    let mut all = Modules::new().register(Arc::new(QuickModule) as Arc<dyn Module>);
    for module in modules.modules() {
        all = all.register(module);
    }
    all.register(Arc::new(EmojiModule) as Arc<dyn Module>)
        .register(Arc::new(EnglishModule) as Arc<dyn Module>)
        .register(Arc::clone(script) as Arc<dyn Module>)
}

/// 生产注册表:宿主功能(短语、联想)居中,快捷输入在前、装饰在后。由宿主
/// 持有简繁句柄,设置变更时调 [`ScriptModule::set_script`]。
pub fn registry_with_script(
    phrases: Option<Arc<CustomPhrases>>,
    prediction: Arc<Prediction>,
    script: Arc<ScriptModule>,
) -> Arc<Modules> {
    Arc::new(complete_builtins(
        registry_inner(phrases, prediction),
        &script,
    ))
}

fn registry_inner(phrases: Option<Arc<CustomPhrases>>, prediction: Arc<Prediction>) -> Modules {
    let mut modules = Modules::new();
    if let Some(phrases) = phrases {
        let module: Arc<dyn Module> = phrases;
        modules = modules.register(module);
    }
    modules.register(prediction as Arc<dyn Module>)
}
