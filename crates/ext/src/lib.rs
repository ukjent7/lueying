//! 掠影输入法扩展 SDK。
//!
//! 扩展分两类:
//!
//! - **输入方案** [`Scheme`]:把击键串展开成规范全拼,再交给核心词图。双拼、
//!   直接辅助码这类会改变音节切分的能力走这里。
//! - **候选模块** [`Module`]:在核心候选生成后增删改候选,或在真正上屏前改写
//!   文本。自定义短语、下一词联想这类后处理能力走这里。
//!
//! 两类扩展都随宿主编译进来,由 `lueying-server` 在启动时注册;运行期不加载
//! 未签名 DLL。扩展作者依赖本 crate 即可,不需要直接依赖引擎内部模块。
//!
//! # 最小模块
//!
//! ```
//! use lueying_ext::{Candidate, CandidateContext, Capability, Manifest, Module};
//!
//! struct Ping;
//!
//! impl Module for Ping {
//!     fn manifest(&self) -> Manifest {
//!         Manifest {
//!             id: "ping",
//!             name: "示例",
//!             version: "1.0",
//!             capabilities: &[Capability::Candidates],
//!         }
//!     }
//!
//!     fn candidates(&self, ctx: &CandidateContext<'_>, cands: &mut Vec<Candidate>) {
//!         if ctx.query == "ping" {
//!             cands.insert(0, Candidate::whole(ctx.raw.len(), "示例", 1.0));
//!         }
//!     }
//! }
//! ```

#![deny(missing_docs)]

pub use lueying_engine::module::{
    CandidateContext, Capability, CommitContext, Manifest, Module, Modules,
};
pub use lueying_engine::scheme::{
    CandidateFilter, Expansion, FullPinyin, Scheme, SchemeManifest,
};
pub use lueying_engine::{
    Candidate, CivilTime, Config, Engine, Event, Fuzzy, Key, Mode, Script, UserDict,
};

/// SDK 版本。扩展可在 `Manifest` 里自行标注兼容的 SDK 版本。
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 扩展作者常用的导入集合。
pub mod prelude {
    pub use crate::{
        Candidate, CandidateContext, CandidateFilter, Capability, CivilTime, CommitContext, Config,
        Engine, Event, Expansion, FullPinyin, Fuzzy, Key, Manifest, Mode, Module, Modules, Script,
        Scheme, SchemeManifest, UserDict, SDK_VERSION,
    };
}
