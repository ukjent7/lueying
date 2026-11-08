use std::sync::Arc;
use std::time::Duration;

use crate::Registry;

use super::*;
use crate::test_guest::{self, FIXTURE_ACCENT};

const WAIT: Duration = Duration::from_secs(5);

fn manifest(id: &str, appearance: bool, commands: &[(&str, &str)]) -> Arc<Manifest> {
    let text = if appearance && commands.is_empty() {
        test_guest::appearance_manifest(id)
    } else {
        test_guest::command_manifest(id, commands)
    };
    Arc::new(Manifest::parse(text.as_bytes()).expect("清单"))
}

fn worker(wasm: &[u8], manifest: Arc<Manifest>, limits: Limits) -> Result<Worker, String> {
    Worker::spawn(wasm.to_vec(), manifest, Arc::new(Registry::new()), limits)
}

#[test]
fn runs_a_command() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "命令回显");
    let limits = Limits::default();
    let worker =
        worker(&wasm, manifest("demo", false, &[("run", "运行")]), limits).expect("启动 worker");
    assert_eq!(worker.command_wait("run", "ping", WAIT), Ok("ping".into()));
}

#[test]
fn reports_guest_error() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "命令报错");
    let limits = Limits::default();
    let worker =
        worker(&wasm, manifest("demo", false, &[("fail", "失败")]), limits).expect("启动 worker");
    let error = worker.command_wait("fail", "boom", WAIT).unwrap_err();
    assert!(error.contains("boom"), "{error}");
}

#[test]
fn traps_and_rebuilds() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "trap 恢复");
    let limits = Limits::default();
    let worker =
        worker(&wasm, manifest("demo", false, &[("trap", "陷阱")]), limits).expect("启动 worker");
    let first = worker.command_wait("trap", "{}", WAIT);
    assert!(first.is_err(), "{first:?}");
    // trap 之后实例照旧可用:第二次调用照常执行并再次 trap。
    let second = worker.command_wait("trap", "{}", WAIT);
    assert!(second.is_err(), "{second:?}");
}

#[test]
fn fuel_stops_a_loop() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "燃料打断");
    let limits = Limits {
        command_fuel: 10_000,
        ..Limits::default()
    };
    let worker =
        worker(&wasm, manifest("demo", false, &[("spin", "空转")]), limits).expect("启动 worker");
    let error = worker.command_wait("spin", "{}", WAIT).unwrap_err();
    let metrics = worker.metrics();
    // 燃料耗尽的 trap 文本不带 "fuel"(只有 wasm backtrace),以超时指标断言。
    let _ = error;
    assert!(metrics.timeouts > 0, "metrics={metrics:?}");
}

#[test]
fn appearance_returns_patch() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "外观补丁");
    let limits = Limits::default();
    let worker = worker(&wasm, manifest("demo", true, &[]), limits).expect("启动 worker");
    let patch = worker
        .appearance(&SceneIn::default())
        .expect("没有被跳过")
        .expect("派发成功")
        .expect("有补丁");
    assert_eq!(patch.accent, Some(FIXTURE_ACCENT));
}

/// 服务失败不许被 guest 吞掉:guest 拿到错误仍返回成功,宿主也要把这次
/// 调用判失败。
#[test]
fn service_failure_fails_the_command() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "服务失败结算");
    let limits = Limits::default();
    let manifest = manifest("demo", false, &[("notify", "通知")]);
    let worker = worker(&wasm, manifest, limits).expect("启动 worker");
    let error = worker.command_wait("notify", "{}", WAIT).unwrap_err();
    assert!(error.contains("没有提供者"), "{error}");
    assert!(error.contains("notify"), "{error}");
}

#[test]
fn rejects_manifest_without_entry() {
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "无入口清单");
    let limits = Limits::default();
    let text = test_guest::plain_manifest("demo");
    let manifest = Arc::new(Manifest::parse(text.as_bytes()).expect("清单"));
    let error = worker(&wasm, manifest, limits).err().expect("应当拒绝");
    assert!(error.contains("没有可调用的入口"), "{error}");
}

#[test]
fn rejects_core_module() {
    // 核心模块(Extism 时代的形态)不再受支持:要打包成组件才能加载。
    // 空核心模块 = 魔数 + 版本段。
    let wasm = b"\x00asm\x01\x00\x00\x00".to_vec();
    let limits = Limits::default();
    let manifest = manifest("demo", false, &[("run", "运行")]);
    let error = worker(&wasm, manifest, limits).err().expect("应当拒绝");
    assert!(error.contains("组件"), "{error}");
}
