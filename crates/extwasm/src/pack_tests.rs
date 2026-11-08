use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::Registry;

use super::*;
use crate::test_guest::{self, FIXTURE_ACCENT, FIXTURE_B_ACCENT, Temp};

const WAIT: Duration = Duration::from_secs(5);

fn load(dir: &Path) -> Pack {
    Pack::load(dir).expect("加载扩展")
}

fn write_command(dir: &Path, wasm: &[u8]) {
    test_guest::write_pack(
        dir,
        &test_guest::command_manifest("demo", &[("run", "运行")]),
        wasm,
    );
}

#[test]
fn loads_and_runs() {
    let temp = Temp::new("run");
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "命令包");
    let dir = temp.pack("demo");
    write_command(&dir, &wasm);
    let mut pack = load(&dir);
    assert_eq!(pack.manifest().id, "demo");
    assert_eq!(pack.invoke_wait("run", "ping", WAIT), Ok("ping".into()));
    assert_eq!(pack.pending(), 0);
    assert!(pack.last_error().is_none());
}

#[test]
fn rejects_undeclared_command() {
    let temp = Temp::new("undeclared");
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "未声明命令");
    let dir = temp.pack("demo");
    write_command(&dir, &wasm);
    let mut pack = load(&dir);
    let error = pack.invoke_wait("other", "{}", WAIT).unwrap_err();
    assert!(error.contains("没有声明"), "{error}");
}

#[test]
fn dispatches_appearance() {
    let temp = Temp::new("appearance");
    let dir = temp.pack("demo");
    test_guest::write_pack(
        &dir,
        &test_guest::appearance_manifest("demo"),
        test_guest::fixture_or_skip!(test_guest::fixture(), "外观包"),
    );
    let mut pack = load(&dir);
    let patch = pack.dispatch(&SceneIn::default()).expect("有补丁");
    assert_eq!(patch.accent, Some(FIXTURE_ACCENT));
}

#[test]
fn private_scene_is_skipped() {
    let temp = Temp::new("private");
    let dir = temp.pack("demo");
    test_guest::write_pack(
        &dir,
        &test_guest::appearance_manifest("demo"),
        test_guest::fixture_or_skip!(test_guest::fixture(), "隐私场景"),
    );
    let mut pack = load(&dir);
    let scene = SceneIn {
        private: true,
        ..SceneIn::default()
    };
    assert!(pack.dispatch(&scene).is_none());
    assert!(pack.last_error().is_none());
}

#[test]
fn three_traps_disable_pack() {
    let temp = Temp::new("traps");
    let dir = temp.pack("demo");
    test_guest::write_pack(
        &dir,
        &test_guest::command_manifest("demo", &[("trap", "陷阱")]),
        test_guest::fixture_or_skip!(test_guest::fixture(), "trap 停用"),
    );
    let mut pack = load(&dir);
    for _ in 0..3 {
        assert!(pack.invoke_wait("trap", "{}", WAIT).is_err());
    }
    assert!(pack.disabled_reason().is_some());
    let error = pack.invoke_wait("trap", "{}", WAIT).unwrap_err();
    assert!(error.contains("已停用"), "{error}");
}

#[test]
fn hot_reload_swaps_wasm() {
    let temp = Temp::new("reload");
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "热重载");
    let dir = temp.pack("demo");
    write_command(&dir, &wasm);
    let mut pack = load(&dir);
    assert_eq!(pack.poll(), Poll::Unchanged);
    std::fs::write(
        dir.join("extension.wasm"),
        test_guest::fixture_or_skip!(test_guest::fixture_b(), "热重载变体"),
    )
    .expect("写新模块");
    assert_eq!(pack.poll(), Poll::Reloaded);
    assert_eq!(pack.invoke_wait("run", "ping", WAIT), Ok("[b]ping".into()));
}

#[test]
fn gate_denial_blocks_load() {
    let temp = Temp::new("gate-load");
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "同意门禁");
    let dir = temp.pack("demo");
    write_command(&dir, &wasm);
    let gate: ConsentGate = Arc::new(|_, _| Err("未同意".into()));
    let error = Pack::load_with_gate(
        &dir,
        Limits::default(),
        Arc::new(Registry::new()),
        Some(gate),
    )
    .err()
    .expect("应当拒绝");
    assert!(error.contains("未同意"), "{error}");
}

#[test]
fn gate_denial_keeps_previous_version() {
    let temp = Temp::new("gate-reload");
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "拒绝更新保留旧版");
    let dir = temp.pack("demo");
    write_command(&dir, &wasm);
    let allowed = Arc::new(AtomicBool::new(true));
    let gate: ConsentGate = {
        let allowed = Arc::clone(&allowed);
        Arc::new(move |_, _| {
            if allowed.load(Ordering::Relaxed) {
                Ok(())
            } else {
                Err("未同意".into())
            }
        })
    };
    let mut pack = Pack::load_with_gate(
        &dir,
        Limits::default(),
        Arc::new(Registry::new()),
        Some(gate),
    )
    .expect("首次加载");
    allowed.store(false, Ordering::Relaxed);
    std::fs::write(dir.join("extension.wasm"), b"not wasm at all").expect("写新模块");
    assert!(matches!(pack.poll(), Poll::Failed(_)));
    assert_eq!(pack.invoke_wait("run", "ping", WAIT), Ok("ping".into()));
}

#[test]
fn removed_manifest_reports_removed() {
    let temp = Temp::new("removed");
    let wasm = test_guest::fixture_or_skip!(test_guest::fixture(), "清单删除");
    let dir = temp.pack("demo");
    write_command(&dir, &wasm);
    let mut pack = load(&dir);
    std::fs::remove_file(dir.join("manifest.json")).expect("删清单");
    assert_eq!(pack.poll(), Poll::Removed);
}

#[test]
fn rejects_missing_wasm() {
    let temp = Temp::new("missing-wasm");
    let dir = temp.pack("demo");
    std::fs::create_dir_all(&dir).expect("建包目录");
    std::fs::write(
        dir.join("manifest.json"),
        test_guest::command_manifest("demo", &[("run", "运行")]),
    )
    .expect("写清单");
    assert!(Pack::load(&dir).is_err());
}

/// 合并顺序的另一半:变体 B 的主题色与 A 不同,后到者覆盖。
#[test]
fn variant_b_appearance_differs() {
    let temp = Temp::new("variant-b");
    let dir = temp.pack("demo");
    test_guest::write_pack(
        &dir,
        &test_guest::appearance_manifest("demo"),
        test_guest::fixture_or_skip!(test_guest::fixture_b(), "外观变体"),
    );
    let mut pack = load(&dir);
    let patch = pack.dispatch(&SceneIn::default()).expect("有补丁");
    assert_eq!(patch.accent, Some(FIXTURE_B_ACCENT));
}
