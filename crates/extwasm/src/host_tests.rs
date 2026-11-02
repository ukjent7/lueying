use std::sync::Arc;
use std::time::Duration;

use lueying_extsvc::Registry;

use super::*;
use crate::test_guest;

const WAIT: Duration = Duration::from_secs(5);

fn manifest(id: &str, appearance: bool, commands: &[(&str, &str)]) -> Arc<Manifest> {
    let text = if appearance && commands.is_empty() {
        test_guest::appearance_manifest(id)
    } else {
        test_guest::command_manifest(id, commands)
    };
    Arc::new(Manifest::parse(text.as_bytes()).expect("清单"))
}

fn worker(wasm: &str, manifest: Arc<Manifest>, limits: Limits) -> Result<Worker, String> {
    let engine = Arc::new(engine(limits)?);
    let module = Arc::new(compile(&engine, wasm.as_bytes())?);
    Worker::spawn(engine, module, manifest, Arc::new(Registry::new()), limits)
}

#[test]
fn runs_a_command() {
    let limits = Limits::default();
    let worker = worker(
        &test_guest::command_guest(r#"{"ok":1}"#),
        manifest("demo", false, &[("run", "运行")]),
        limits,
    )
    .expect("启动 worker");
    assert_eq!(
        worker.command_wait("run", "{}", WAIT),
        Ok(r#"{"ok":1}"#.into())
    );
}

#[test]
fn reports_guest_error() {
    let limits = Limits::default();
    let worker = worker(
        &test_guest::error_guest("boom"),
        manifest("demo", false, &[("run", "运行")]),
        limits,
    )
    .expect("启动 worker");
    assert_eq!(worker.command_wait("run", "{}", WAIT), Err("boom".into()));
}

#[test]
fn traps_and_rebuilds() {
    let limits = Limits::default();
    let worker = worker(
        &test_guest::trap_guest(),
        manifest("demo", false, &[("run", "运行")]),
        limits,
    )
    .expect("启动 worker");
    let first = worker.command_wait("run", "{}", WAIT);
    assert!(first.is_err(), "{first:?}");
    let second = worker.command_wait("run", "{}", WAIT);
    assert!(second.is_err(), "{second:?}");
}

#[test]
fn fuel_stops_a_loop() {
    let limits = Limits {
        command_fuel: 10_000,
        ..Limits::default()
    };
    let worker = worker(
        &test_guest::loop_guest(),
        manifest("demo", false, &[("run", "运行")]),
        limits,
    )
    .expect("启动 worker");
    let error = worker.command_wait("run", "{}", WAIT).unwrap_err();
    let metrics = worker.metrics();
    assert!(metrics.timeouts > 0, "error={error:?} metrics={metrics:?}");
}

#[test]
fn appearance_returns_patch() {
    let limits = Limits::default();
    let worker = worker(
        &test_guest::appearance_guest(r#"{"accent":1193046}"#),
        manifest("demo", true, &[]),
        limits,
    )
    .expect("启动 worker");
    let patch = worker
        .appearance(&SceneIn::default())
        .expect("没有被跳过")
        .expect("派发成功")
        .expect("有补丁");
    assert_eq!(patch.accent, Some(1_193_046));
}

#[test]
fn rejects_missing_command_export() {
    let limits = Limits::default();
    let error = worker(
        &test_guest::appearance_guest("{}"),
        manifest("demo", false, &[("run", "运行")]),
        limits,
    )
    .err()
    .expect("应当拒绝");
    assert!(error.contains("lueying_command"), "{error}");
}

#[test]
fn rejects_missing_appearance_export() {
    let limits = Limits::default();
    let error = worker(
        &test_guest::command_guest("{}"),
        manifest("demo", true, &[]),
        limits,
    )
    .err()
    .expect("应当拒绝");
    assert!(error.contains("lueying_appearance"), "{error}");
}

#[test]
fn rejects_missing_allocator() {
    let wasm = r#"(module
      (memory (export "memory") 1)
      (func (export "lueying_init") (result i32) (i32.const 0)))"#;
    let limits = Limits::default();
    let error = worker(wasm, manifest("demo", false, &[("run", "运行")]), limits)
        .err()
        .expect("应当拒绝");
    assert!(error.contains("lueying_alloc"), "{error}");
}
