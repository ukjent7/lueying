//! 运行期脚本扩展:用户写 TS/JS,输入法在沙箱里跑它,可以改候选窗外观,
//! 也可以提供命令。
//!
//! 扩展是一个目录(`%APPDATA%\lueying\extensions\<id>\`),里面一份
//! `manifest.json` 与一份入口脚本。入口脚本由用户自己的 esbuild 打成单文件
//! IIFE,输入法不内置转译器;运行期不加载 DLL,也没有 `import`。
//!
//! 扩展能看见的只有面板状态([`SceneIn`]),能改的只有主题色与三个 SkSL
//! 槽位([`ScenePatch`])。候选文字、击键历史、宿主应用名都不进 JS。命令
//! 只能执行扩展自己声明过的短 id,完整 id 由 [`Runtime`] 加上扩展前缀。
//! 清单 `needs` 里声明过的服务经 `lueying.<service>.*` 调用,Promise 结算,
//! 调用面在引导层按 needs 门控;接口记录在 `lueying-extsvc` 的
//! `lueying.wit`。
//!
//! # 边界
//!
//! - 内存上限 [`Limits::memory_bytes`],栈上限 [`Limits::stack_bytes`]。
//! - 初始化与每次派发都有指令时限,超时由 QuickJS 中断。
//! - 命令可以返回 Promise:等待 `lueying.timeout` 与原生服务不占预算,
//!   每个作业步仍受派发时限约束,反复入队的任务每轮泵至多 256 步。
//! - 密码/隐私字段([`SceneIn::private`])直接跳过,脚本不会收到那次状态。
//! - 高对比度下补丁里的效果被摘掉,只留主题色。
//! - 连续 [`MAX_FAULTS`] 次失败停用该包,重载成功后恢复。
//!
//! # 示例
//!
//! ```
//! use lueying_extjs::{Pack, SceneIn};
//!
//! fn demo(dir: &std::path::Path) {
//!     let mut pack = Pack::load(dir).expect("扩展加载失败");
//!     let state = SceneIn { accent: 0x2f6fed, ..SceneIn::default() };
//!     if let Some(patch) = pack.dispatch(&state) {
//!         println!("主题色 {:06x}", patch.accent.unwrap_or(state.accent));
//!     }
//! }
//! ```

#![deny(missing_docs)]

pub mod bindings;
mod host;
mod manifest;
mod pack;
mod runtime;
mod scene;

pub use bindings::{Binding, BindingTable, Hotkey, Input, Modifier};
pub use host::{Host, Invoke, Limits, Settled};
pub use lueying_extsvc::{Completion, Done, Provider, Registry};
pub use manifest::{
    Activate, Command, MAX_MANIFEST_BYTES, MAX_SOURCE_BYTES, Manifest, Permission, Service, When,
};
pub use pack::{Pack, Poll};
pub use runtime::{CommandInfo, IDLE_UNLOAD, Runtime, Scope};
pub use scene::{EffectSlot, EffectsPatch, MAX_EFFECT_BYTES, SceneIn, ScenePatch};

/// 场景协议版本。脚本在清单里声明依赖的版本,高于本值拒绝加载。
pub const SCENE_API: u32 = 1;

/// 连续失败多少次停用扩展包;重载成功后计数清零。
pub const MAX_FAULTS: u32 = 3;
