use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lueying_extsvc::Registry;

use super::*;
use crate::test_guest::{self, Temp};

const WAIT: Duration = Duration::from_secs(5);

fn load(dir: &Path) -> Pack {
    Pack::load(dir).expect("加载扩展")
}

fn write_command(dir: &Path, result: &str) {
    test_guest::write_pack(
        dir,
        &test_guest::command_manifest("demo", &[("run", "运行")]),
        &test_guest::command_guest(result),
    );
}

#[test]
fn loads_and_runs() {
    let temp = Temp::new("run");
    let dir = temp.pack("demo");
    write_command(&dir, r#"{"ok":1}"#);
    let mut pack = load(&dir);
    assert_eq!(pack.manifest().id, "demo");
    assert_eq!(
        pack.invoke_wait("run", "{}", WAIT),
        Ok(r#"{"ok":1}"#.into())
    );
    assert_eq!(pack.pending(), 0);
    assert!(pack.last_error().is_none());
}

#[test]
fn rejects_undeclared_command() {
    let temp = Temp::new("undeclared");
    let dir = temp.pack("demo");
    write_command(&dir, "{}");
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
        &test_guest::appearance_guest(r#"{"accent":1193046}"#),
    );
    let mut pack = load(&dir);
    let patch = pack.dispatch(&SceneIn::default()).expect("有补丁");
    assert_eq!(patch.accent, Some(1_193_046));
}

#[test]
fn high_contrast_drops_effects() {
    let temp = Temp::new("contrast");
    let dir = temp.pack("demo");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&test_guest::command_manifest("demo", &[("run", "运行")]))
            .expect("清单 JSON");
    manifest["appearance"] = serde_json::json!(true);
    test_guest::write_pack(
        &dir,
        &manifest.to_string(),
        &test_guest::command_appearance_guest(
            "{}",
            r#"{"accent":1193046,"effects":{"background":"half4(1)"}}"#,
        ),
    );
    let mut pack = load(&dir);
    let scene = SceneIn {
        high_contrast: true,
        ..SceneIn::default()
    };
    let patch = pack.dispatch(&scene).expect("有补丁");
    assert_eq!(patch.accent, Some(1_193_046));
    assert!(patch.effects.is_none());
}

#[test]
fn private_scene_is_skipped() {
    let temp = Temp::new("private");
    let dir = temp.pack("demo");
    test_guest::write_pack(
        &dir,
        &test_guest::appearance_manifest("demo"),
        &test_guest::appearance_guest("{}"),
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
        &test_guest::command_manifest("demo", &[("run", "运行")]),
        &test_guest::trap_guest(),
    );
    let mut pack = load(&dir);
    for _ in 0..3 {
        assert!(pack.invoke_wait("run", "{}", WAIT).is_err());
    }
    assert!(pack.disabled_reason().is_some());
    let error = pack.invoke_wait("run", "{}", WAIT).unwrap_err();
    assert!(error.contains("已停用"), "{error}");
}

#[test]
fn hot_reload_swaps_wasm() {
    let temp = Temp::new("reload");
    let dir = temp.pack("demo");
    write_command(&dir, "{}");
    let mut pack = load(&dir);
    assert_eq!(pack.poll(), Poll::Unchanged);
    std::fs::write(dir.join("extension.wasm"), test_guest::command_guest("v2")).expect("写新模块");
    assert_eq!(pack.poll(), Poll::Reloaded);
    assert_eq!(pack.invoke_wait("run", "{}", WAIT), Ok("v2".into()));
}

#[test]
fn gate_denial_blocks_load() {
    let temp = Temp::new("gate-load");
    let dir = temp.pack("demo");
    write_command(&dir, "{}");
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
    let dir = temp.pack("demo");
    write_command(&dir, "v1");
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
    std::fs::write(dir.join("extension.wasm"), test_guest::command_guest("v2")).expect("写新模块");
    assert!(matches!(pack.poll(), Poll::Failed(_)));
    assert_eq!(pack.invoke_wait("run", "{}", WAIT), Ok("v1".into()));
}

#[test]
fn removed_manifest_reports_removed() {
    let temp = Temp::new("removed");
    let dir = temp.pack("demo");
    write_command(&dir, "{}");
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
