//! 运行期扩展自检:建一个临时扩展包,验证加载、派发、隐私与高对比度。
//!
//! 只在 `LUEYING_D2D_SELFTEST` 下跑(CI),结论写日志供 win-e2e 断言。

use std::path::Path;

use lueying_extwasm::{Pack, SceneIn};
use lueying_server::config;

/// 外观补丁:固定主题色,便于断言隐私与高对比度路径。
const PATCH: &str = r#"{"accent":1056816}"#;

/// 最小 Wasm guest:extism 内核分配加固定外观。
fn guest() -> String {
    let mut stores = String::new();
    for (index, byte) in PATCH.as_bytes().iter().enumerate() {
        stores.push_str(&format!(
            "    (call $store_u8 (i64.add (local.get $ptr) (i64.const {index})) (i32.const {byte}))\n"
        ));
    }
    format!(
        r#"(module
  (import "extism:host/env" "alloc" (func $alloc (param i64) (result i64)))
  (import "extism:host/env" "store_u8" (func $store_u8 (param i64 i32)))
  (import "extism:host/env" "output_set" (func $output_set (param i64 i64)))
  (func (export "lueying_appearance") (result i32)
    (local $ptr i64)
    (local.set $ptr (call $alloc (i64.const {len})))
{stores}    (call $output_set (local.get $ptr) (i64.const {len}))
    (i32.const 0)))"#,
        len = PATCH.len(),
        stores = stores,
    )
}

/// 清单;id 用进程号,避免和用户自己的扩展撞名。
fn manifest(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"自检扩展","version":"1.0.0","api": 4,"wasm":"extension.wasm","appearance":true}}"#
    )
}

pub(super) fn run() {
    match check() {
        Ok(()) => println!("扩展自检通过:加载、主题色、隐私、高对比度与设置列表"),
        Err(e) => println!("扩展自检失败:{e}"),
    }
}

fn check() -> Result<(), String> {
    let root = config::extensions_dir().ok_or("APPDATA 缺失")?;
    let id = format!("selftest-{}", std::process::id());
    let dir = root.join(&id);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建包目录: {e}"))?;
    let written = std::fs::write(dir.join("manifest.json"), manifest(&id))
        .and_then(|()| std::fs::write(dir.join("extension.wasm"), guest()));
    let outcome = written
        .map_err(|e| format!("写扩展包: {e}"))
        .and_then(|()| check_pack(&dir))
        .and_then(|()| check_listing(&id));
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

/// 设置页的扩展列表要能读到这个包(只读清单,不加载 Wasm)。
fn check_listing(id: &str) -> Result<(), String> {
    let listed = lueying_server::settings::actions::installed_scripts();
    if listed.iter().any(|script| script.id == id) {
        Ok(())
    } else {
        Err(format!("设置页没有列出扩展 {id}"))
    }
}

fn check_pack(dir: &Path) -> Result<(), String> {
    let mut pack = Pack::load(dir).map_err(|e| format!("加载: {e}"))?;
    let first = pack
        .dispatch(&SceneIn::default())
        .ok_or("普通场景没有补丁")?;
    if first.accent != Some(0x10_20_30) {
        return Err(format!("首次派发主题色 {:?}", first.accent));
    }
    if pack
        .dispatch(&SceneIn {
            private: true,
            ..SceneIn::default()
        })
        .is_some()
    {
        return Err("隐私场景仍然派发".into());
    }
    let patch = pack
        .dispatch(&SceneIn::default())
        .ok_or("隐私场景后没有补丁")?;
    if patch.accent != Some(0x10_20_30) {
        return Err(format!("隐私场景后主题色 {:?}", patch.accent));
    }
    let patch = pack
        .dispatch(&SceneIn {
            high_contrast: true,
            ..SceneIn::default()
        })
        .ok_or("高对比度场景没有补丁")?;
    if patch.accent != Some(0x10_20_30) {
        return Err(format!("高对比度补丁 {:?}", patch.accent));
    }
    Ok(())
}
