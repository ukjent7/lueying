//! 宿主侧功能模块组合点。
//!
//! 核心引擎定义 `Module` 契约;高级能力(双拼、辅助码、繁简、自定义短语、
//! 符号表、下一词联想)各自作为独立模块实现,在这里按希望的顺序注册。
//! 默认不注册任何模块,现有输入路径与性能保持不变。
//!
//! 模块随安装包编译进来,不从用户目录加载 DLL:输入法进程运行在宿主应用里,
//! 运行期加载未签名代码的代价高于收益。

use std::sync::Arc;

use lueying_engine::module::Modules;

pub(super) fn registry() -> Arc<Modules> {
    let modules = Modules::new();
    // 以后在这里组合模块,例如:
    // let modules = modules.register(Arc::new(lueying_features::shuangpin(...)));
    Arc::new(modules)
}
