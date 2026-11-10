//! 宿主侧功能模块组合点。
//!
//! 核心引擎定义 `Module` 契约;自定义短语、下一词联想等能力由
//! `lueying-core` 的功能模块实现,在这里按希望的顺序注册。模块随安装包编译进来,
//! 不从用户目录加载 DLL:输入法进程运行在宿主应用里,运行期加载未签名代码
//! 的代价高于收益。

use std::sync::Arc;

use lueying_core::features::{CustomPhrases, Prediction, ScriptModule};
use lueying_core::module::Modules;

pub(super) fn registry(
    phrases: Option<Arc<CustomPhrases>>,
    prediction: Arc<Prediction>,
    script: Arc<ScriptModule>,
) -> Arc<Modules> {
    lueying_core::features::registry_with_script(phrases, prediction, script)
}
