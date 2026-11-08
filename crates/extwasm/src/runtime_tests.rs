use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use crate::Registry;

use super::*;
use crate::Limits;
use crate::test_guest::{self, FIXTURE_ACCENT, FIXTURE_B_ACCENT, Temp};

const WAIT: Duration = Duration::from_secs(5);

fn runtime(temp: &Temp, order: &[&str]) -> (Runtime, Vec<String>) {
    let ids: Vec<String> = order.iter().map(|id| (*id).to_string()).collect();
    Runtime::open_with(
        temp.root(),
        &ids,
        Limits::default(),
        Arc::new(Registry::new()),
    )
}

fn write_command_pack(temp: &Temp, id: &str, wasm: &[u8]) {
    test_guest::write_pack(
        &temp.pack(id),
        &test_guest::command_manifest(id, &[("run", "运行")]),
        wasm,
    );
}

fn write_appearance_pack(temp: &Temp, id: &str, wasm: &[u8]) {
    test_guest::write_pack(&temp.pack(id), &test_guest::appearance_manifest(id), wasm);
}

#[test]
fn lists_commands_without_loading() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "命令列表");
    let temp = Temp::new("runtime-list");
    write_command_pack(&temp, "demo", &wasm);
    let (runtime, errors) = runtime(&temp, &["demo"]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(runtime.loaded_count(), 0);
    let commands = runtime.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].id, "demo.run");
    assert_eq!(commands[0].title, "运行");
    assert_eq!(runtime.scope("demo").expect("作用域").commands, ["run"]);
}

#[test]
fn invoke_loads_and_runs() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "按需加载");
    let temp = Temp::new("runtime-invoke");
    write_command_pack(&temp, "demo", &wasm);
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    assert_eq!(
        runtime.invoke_wait("demo.run", "ping", WAIT),
        Ok("ping".into())
    );
    assert!(runtime.loaded("demo"));
    assert_eq!(runtime.loaded_count(), 1);
}

#[test]
fn unknown_command_is_rejected() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "未知命令");
    let temp = Temp::new("runtime-unknown");
    write_command_pack(&temp, "demo", &wasm);
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    assert!(runtime.invoke_wait("demo.other", "{}", WAIT).is_err());
    assert!(runtime.invoke_wait("missing.run", "{}", WAIT).is_err());
    assert!(runtime.invoke_wait("noprefix", "{}", WAIT).is_err());
}

#[test]
fn idle_unload_drops_command_pack() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "空闲卸载");
    let temp = Temp::new("runtime-idle");
    write_command_pack(&temp, "demo", &wasm);
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    runtime.activate("demo").expect("激活");
    assert!(runtime.loaded("demo"));
    runtime.set_idle(Duration::ZERO);
    runtime.idle_unload_at(Instant::now() + Duration::from_secs(1));
    assert!(!runtime.loaded("demo"));
    assert_eq!(runtime.commands().len(), 1);
}

#[test]
fn appearance_pack_is_resident() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "外观常驻");
    let temp = Temp::new("runtime-resident");
    write_appearance_pack(&temp, "demo", &wasm);
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    let patch = runtime.dispatch(&SceneIn::default()).expect("有补丁");
    assert_eq!(patch.accent, Some(FIXTURE_ACCENT));
    assert!(runtime.loaded("demo"));
    runtime.set_idle(Duration::ZERO);
    runtime.idle_unload_at(Instant::now() + Duration::from_secs(1));
    assert!(runtime.loaded("demo"));
}

#[test]
fn dispatch_merges_in_order() {
    let wasm_a = test_guest::fixture_or_skip!(test_guest::fixture(), "合并顺序 A");
    let wasm_b = test_guest::fixture_or_skip!(test_guest::fixture_b(), "合并顺序 B");
    let temp = Temp::new("runtime-merge");
    write_appearance_pack(&temp, "first", &wasm_a);
    write_appearance_pack(&temp, "second", &wasm_b);
    let (mut runtime, _) = runtime(&temp, &["first", "second"]);
    let patch = runtime.dispatch(&SceneIn::default()).expect("有补丁");
    assert_eq!(patch.accent, Some(FIXTURE_B_ACCENT));
}

#[test]
fn private_scene_is_skipped() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "隐私跳过");
    let temp = Temp::new("runtime-private");
    write_appearance_pack(&temp, "demo", &wasm);
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    let scene = SceneIn {
        private: true,
        ..SceneIn::default()
    };
    assert!(runtime.dispatch(&scene).is_none());
    assert!(!runtime.loaded("demo"));
}

#[test]
fn poll_removes_deleted_manifest() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "清单删除卸载");
    let temp = Temp::new("runtime-poll");
    write_command_pack(&temp, "demo", &wasm);
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    runtime.activate("demo").expect("激活");
    std::fs::remove_file(temp.pack("demo").join("manifest.json")).expect("删清单");
    assert_eq!(runtime.poll(), 1);
    assert!(runtime.scope("demo").is_none());
}

#[test]
fn duplicate_ids_keep_one_slot() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "重名去重");
    let temp = Temp::new("runtime-duplicate");
    write_command_pack(&temp, "demo", &wasm);
    let (runtime, _) = runtime(&temp, &["demo", "demo"]);
    assert_eq!(runtime.commands().len(), 1);
}

#[test]
fn bad_manifest_is_reported() {
    let temp = Temp::new("runtime-bad");
    let dir = temp.pack("demo");
    std::fs::create_dir_all(&dir).expect("建包目录");
    std::fs::write(dir.join("manifest.json"), "{").expect("写清单");
    let (runtime, errors) = runtime(&temp, &["demo"]);
    assert_eq!(errors.len(), 1);
    assert!(runtime.commands().is_empty());
}
