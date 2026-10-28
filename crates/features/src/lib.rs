//! 掠影第一批内置扩展:双拼、自定义短语、辅助码、下一词联想。
//!
//! 这些能力都通过 [`lueying_ext`] 的公开契约接入,核心引擎只保留全拼、
//! 候选生成与学习。宿主按配置选择方案并注册模块。

pub mod aux_code;
pub mod phrases;
pub mod predict;
pub mod shuangpin;

use std::sync::Arc;

pub use aux_code::{AuxCode, AuxLookup, AuxScheme};
pub use phrases::CustomPhrases;
pub use predict::Prediction;
pub use shuangpin::Shuangpin;

use lueying_ext::{FullPinyin, Module, Modules, Scheme};

/// 宿主可选输入方案。`Full` 是默认值,其余为双拼。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SchemeId {
    #[default]
    Full,
    Xiaohe,
    Ziranma,
    Mspy,
    Sogou,
}

impl SchemeId {
    pub const ALL: [Self; 5] = [
        Self::Full,
        Self::Xiaohe,
        Self::Ziranma,
        Self::Mspy,
        Self::Sogou,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Xiaohe => "xiaohe",
            Self::Ziranma => "ziranma",
            Self::Mspy => "mspy",
            Self::Sogou => "sogou",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "全拼",
            Self::Xiaohe => "小鹤",
            Self::Ziranma => "自然码",
            Self::Mspy => "微软",
            Self::Sogou => "搜狗",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "xiaohe" | "flypy" => Self::Xiaohe,
            "ziranma" | "zrm" => Self::Ziranma,
            "mspy" | "microsoft" => Self::Mspy,
            "sogou" => Self::Sogou,
            _ => Self::Full,
        }
    }
}

/// 构造方案;`aux` 为真时在基础方案上叠加辅助码。
pub fn scheme(id: SchemeId, aux: bool) -> Arc<dyn Scheme> {
    let base: Arc<dyn Scheme> = match id {
        SchemeId::Full => Arc::new(FullPinyin),
        _ => Arc::new(Shuangpin::by_id(id.as_str()).expect("SchemeId 与双拼布局表必须一一对应")),
    };
    if aux {
        Arc::new(AuxScheme::new(base, AuxCode::builtin()))
    } else {
        base
    }
}

/// 组合第一批内置模块。自定义短语按宿主给出的文件创建;联想默认启用。
pub fn registry(phrases: Option<Arc<CustomPhrases>>) -> Arc<Modules> {
    registry_with_prediction(phrases, Arc::new(Prediction::new()))
}

/// 与 [`registry`] 相同,但由宿主持有联想开关,设置变更时无需重建注册表。
pub fn registry_with_prediction(
    phrases: Option<Arc<CustomPhrases>>,
    prediction: Arc<Prediction>,
) -> Arc<Modules> {
    let mut modules = Modules::new();
    if let Some(phrases) = phrases {
        let module: Arc<dyn Module> = phrases;
        modules = modules.register(module);
    }
    let module: Arc<dyn Module> = prediction;
    modules = modules.register(module);
    Arc::new(modules)
}
