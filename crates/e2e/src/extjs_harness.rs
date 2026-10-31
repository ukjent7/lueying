//! 脚本扩展 E2E 公共件:检查记录、临时扩展包与加载断言。

use std::path::Path;

use lueying_extjs::Pack;

pub(crate) fn manifest(id: &str, api: u32) -> String {
    format!(r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":{api}}}"#)
}

pub(crate) fn manifest_extra(id: &str, api: u32, extra: &str) -> String {
    format!(r#"{{"id":"{id}","name":"测试","version":"1.0.0","api":{api},{extra}}}"#)
}

pub(crate) fn write_pack(dir: &Path, manifest: &str, source: &str) {
    std::fs::write(dir.join("manifest.json"), manifest).expect("写清单");
    std::fs::write(dir.join("index.js"), source).expect("写脚本");
}

pub(crate) fn load_or_fail(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    dir: &Path,
) -> Option<Pack> {
    match Pack::load(dir) {
        Ok(pack) => Some(pack),
        Err(e) => {
            push(checks, name, false, e);
            None
        }
    }
}
