//! 宿主侧功能模块组合点。
//!
//! 核心引擎定义 `Module` 契约;自定义短语、下一词联想等能力由
//! `lueying-features` 实现,在这里按希望的顺序注册。模块随安装包编译进来,
//! 不从用户目录加载 DLL:输入法进程运行在宿主应用里,运行期加载未签名代码
//! 的代价高于收益。

use std::sync::Arc;

use lueying_engine::module::Modules;
use lueying_features::{CustomPhrases, Prediction};

pub(super) fn registry(
    phrases: Option<Arc<CustomPhrases>>,
    prediction: Arc<Prediction>,
) -> Arc<Modules> {
    lueying_features::registry_with_prediction(phrases, prediction)
}
