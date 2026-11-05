//! 设置页的共享部分:页面模型、设置项的读写与各类动作。

#[cfg(windows)]
pub mod actions;
pub mod consent;
pub mod ext;
pub mod model;
#[cfg(windows)]
pub mod official;
pub mod schema;
pub mod secret;
mod text;
pub mod view;
