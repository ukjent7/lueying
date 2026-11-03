//! 官方输入方案扩展:双拼、形码与辅助码。
//!
//! 这些方案通过 `lueying-ext` 的 `Scheme` 编译期契约接入,可以在宿主构建时
//! 选择是否注册;核心引擎只保留全拼、简拼与全简混输,不包含任何双拼或
//! 字形逻辑。官方默认启用,用户也可以在设置里切回全拼。
//!
//! - [`Shuangpin`]:小鹤、自然码、微软、搜狗四套双拼布局。
//! - [`CodeScheme`]:五笔、仓颉等按字形取码的形码方案。
//! - [`AuxScheme`]:拼音后追加 1–2 位字形码的辅助码过滤。

pub mod aux_code;
pub mod code;
pub mod shuangpin;

use std::sync::Arc;

pub use aux_code::{AuxCode, AuxLookup, AuxScheme};
pub use code::{CodeEntry, CodeScheme, CodeTable};
pub use shuangpin::Shuangpin;

use lueying_ext::{FullPinyin, Scheme};

/// 宿主可选输入方案。`Full` 是默认值,其余为双拼或形码扩展。
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

    /// 配置里的方案名;无法识别的值退回全拼。
    pub fn parse(value: &str) -> Self {
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
