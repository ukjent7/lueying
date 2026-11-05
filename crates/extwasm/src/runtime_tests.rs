use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Registry;

use super::*;
use crate::Limits;
use crate::test_guest::{self, Temp};

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

fn write_command_pack(temp: &Temp, id: &str, result: &str) {
    test_guest::write_pack(
        &temp.pack(id),
        &test_guest::command_manifest(id, &[("run", "运行")]),
        &test_guest::command_guest(result),
    );
}

fn write_appearance_pack(temp: &Temp, id: &str, accent: u32) {
    let patch = format!(r#"{{"accent":{accent}}}"#);
    test_guest::write_pack(
        &temp.pack(id),
        &test_guest::appearance_manifest(id),
        &test_guest::appearance_guest(&patch),
    );
}

#[test]
fn lists_commands_without_loading() {
    let temp = Temp::new("runtime-list");
    write_command_pack(&temp, "demo", "{}");
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
    let temp = Temp::new("runtime-invoke");
    write_command_pack(&temp, "demo", "v1");
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    assert_eq!(runtime.invoke_wait("demo.run", "{}", WAIT), Ok("v1".into()));
    assert!(runtime.loaded("demo"));
    assert_eq!(runtime.loaded_count(), 1);
}

#[test]
fn unknown_command_is_rejected() {
    let temp = Temp::new("runtime-unknown");
    write_command_pack(&temp, "demo", "{}");
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    assert!(runtime.invoke_wait("demo.other", "{}", WAIT).is_err());
    assert!(runtime.invoke_wait("missing.run", "{}", WAIT).is_err());
    assert!(runtime.invoke_wait("noprefix", "{}", WAIT).is_err());
}

#[test]
fn idle_unload_drops_command_pack() {
    let temp = Temp::new("runtime-idle");
    write_command_pack(&temp, "demo", "{}");
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
    let temp = Temp::new("runtime-resident");
    write_appearance_pack(&temp, "demo", 1_193_046);
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    let patch = runtime.dispatch(&SceneIn::default()).expect("有补丁");
    assert_eq!(patch.accent, Some(1_193_046));
    assert!(runtime.loaded("demo"));
    runtime.set_idle(Duration::ZERO);
    runtime.idle_unload_at(Instant::now() + Duration::from_secs(1));
    assert!(runtime.loaded("demo"));
}

#[test]
fn dispatch_merges_in_order() {
    let temp = Temp::new("runtime-merge");
    write_appearance_pack(&temp, "first", 0x11_2233);
    write_appearance_pack(&temp, "second", 0x44_5566);
    let (mut runtime, _) = runtime(&temp, &["first", "second"]);
    let patch = runtime.dispatch(&SceneIn::default()).expect("有补丁");
    assert_eq!(patch.accent, Some(0x44_5566));
}

#[test]
fn private_scene_is_skipped() {
    let temp = Temp::new("runtime-private");
    write_appearance_pack(&temp, "demo", 1);
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
    let temp = Temp::new("runtime-poll");
    write_command_pack(&temp, "demo", "{}");
    let (mut runtime, _) = runtime(&temp, &["demo"]);
    runtime.activate("demo").expect("激活");
    std::fs::remove_file(temp.pack("demo").join("manifest.json")).expect("删清单");
    assert_eq!(runtime.poll(), 1);
    assert!(runtime.scope("demo").is_none());
}

#[test]
fn duplicate_ids_keep_one_slot() {
    let temp = Temp::new("runtime-duplicate");
    write_command_pack(&temp, "demo", "{}");
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
