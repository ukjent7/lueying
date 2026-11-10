//! 扩展声明式设置的读值与清单读取:服务端 `settings.get` / `settings.set` 共用。
//!
//! 设置窗只负责画页面与点击改值;值从哪里来、密钥怎么并入,都在这里,
//! 服务端与设置窗读到的是同一份规则。

use std::collections::BTreeMap;

use lueying_ext::manifest::{Manifest, Setting};
use serde_json::Value;

use crate::config;
use crate::ext_secret;

/// 扩展设置的完整值:配置里的值,缺省补清单默认值;密钥从私密存储并进。
pub fn values(ext: &str, settings: &[Setting], stored: Option<&BTreeMap<String, Value>>) -> Value {
    let mut object = serde_json::Map::new();
    for setting in settings {
        let value = setting.resolve(stored.and_then(|table| table.get(setting.key())));
        object.insert(setting.key().to_string(), value.to_json());
    }
    ext_secret::merge(ext, settings, &mut object);
    Value::Object(object)
}

/// 读已安装扩展的声明式设置;包不在、id 不合法或清单坏时 None。
pub fn manifest_settings(id: &str) -> Option<Vec<Setting>> {
    if !config::valid_script_id(id) {
        return None;
    }
    let path = config::extensions_dir()?.join(id).join("manifest.json");
    let bytes = std::fs::read(path).ok()?;
    Manifest::parse(&bytes)
        .ok()
        .map(|manifest| manifest.settings)
}
