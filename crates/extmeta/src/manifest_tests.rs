use super::*;
use serde_json::json;

fn base() -> serde_json::Value {
    json!({
        "id": "my-pack",
        "name": "我的扩展",
        "version": "1.0.0",
        "appearance": { "api": 1, "main": "index.js" }
    })
}

fn legacy() -> serde_json::Value {
    json!({
        "id": "old-pack",
        "name": "旧包",
        "version": "1.0.0",
        "api": 1
    })
}

fn parse(value: serde_json::Value) -> Result<Manifest, String> {
    Manifest::parse(value.to_string().as_bytes())
}

#[test]
fn parses_legacy_appearance() {
    let manifest = parse(legacy()).expect("旧版清单应通过");
    assert_eq!(manifest.appearance_main(), Some("index.js"));
    assert_eq!(manifest.appearance_api(), Some(1));
    assert!(manifest.is_appearance_only());
}

#[test]
fn parses_multi_component() {
    let mut value = base();
    value["logic"] = json!({
        "api": 1,
        "module": "logic.wasm",
        "permissions": ["candidate-text"]
    });
    value["panel"] = json!({
        "api": 1,
        "command": "panel.exe",
        "permissions": ["screen-capture", "ocr"]
    });
    let manifest = parse(value).expect("多组件清单应通过");
    assert_eq!(
        manifest.logic.as_ref().expect("逻辑组件").permissions,
        vec![Permission::CandidateText]
    );
    assert_eq!(
        manifest.panel.as_ref().expect("能力组件").permissions,
        vec![Permission::ScreenCapture, Permission::Ocr]
    );
    assert!(!manifest.is_appearance_only());
    assert_eq!(manifest.appearance_main(), Some("index.js"));
}

#[test]
fn rejects_mixed_legacy_and_new() {
    let mut value = base();
    value["api"] = json!(1);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_empty_manifest() {
    let value = json!({ "id": "empty", "name": "空包", "version": "1.0.0" });
    assert!(parse(value).is_err());
}

#[test]
fn rejects_unknown_field() {
    let mut value = base();
    value["extra"] = json!(true);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_bad_id() {
    let long = "a".repeat(49);
    for id in ["", "A", "-a", "_a", "a/b", "a b", long.as_str()] {
        let mut value = base();
        value["id"] = json!(id);
        assert!(parse(value).is_err(), "{id:?}");
    }
}

#[test]
fn rejects_bad_name() {
    let long = "a".repeat(81);
    for name in ["", "   ", "a\nb", long.as_str()] {
        let mut value = base();
        value["name"] = json!(name);
        assert!(parse(value).is_err(), "{name:?}");
    }
}

#[test]
fn rejects_bad_version() {
    let long = "a".repeat(33);
    for version in ["", "-1", "1 0", long.as_str()] {
        let mut value = base();
        value["version"] = json!(version);
        assert!(parse(value).is_err(), "{version:?}");
    }
}

#[test]
fn rejects_bad_appearance_entry() {
    let long = format!("{}.js", "a".repeat(62));
    let bad = [
        "../x.js",
        "a/b.js",
        ".hidden.js",
        "x.txt",
        "x.js/",
        "x.JS",
        long.as_str(),
    ];
    for main in bad {
        let mut value = base();
        value["appearance"]["main"] = json!(main);
        assert!(parse(value).is_err(), "{main:?}");
    }
}

#[test]
fn rejects_bad_logic_module() {
    for module in ["../x.wasm", "a/b.wasm", ".hidden.wasm", "x.js", "x.wasm/"] {
        let mut value = base();
        value["logic"] = json!({ "api": 1, "module": module });
        assert!(parse(value).is_err(), "{module:?}");
    }
}

#[test]
fn rejects_bad_panel_command() {
    for command in ["../x.exe", "a/b.exe", ".hidden.exe", "x.wasm", "x.exe/"] {
        let mut value = base();
        value["panel"] = json!({ "api": 1, "command": command });
        assert!(parse(value).is_err(), "{command:?}");
    }
}

#[test]
fn rejects_api_out_of_range() {
    for api in [0, 2] {
        let mut value = base();
        value["appearance"]["api"] = json!(api);
        assert!(parse(value).is_err(), "appearance api={api}");

        let mut value = base();
        value["logic"] = json!({ "api": api, "module": "logic.wasm" });
        assert!(parse(value).is_err(), "logic api={api}");

        let mut value = base();
        value["panel"] = json!({ "api": api, "command": "panel.exe" });
        assert!(parse(value).is_err(), "panel api={api}");
    }
}

#[test]
fn rejects_permission_on_wrong_component() {
    let mut value = base();
    value["logic"] = json!({
        "api": 1,
        "module": "logic.wasm",
        "permissions": ["screen-capture"]
    });
    assert!(parse(value).is_err());

    let mut value = base();
    value["panel"] = json!({
        "api": 1,
        "command": "panel.exe",
        "permissions": ["candidate-text"]
    });
    assert!(parse(value).is_err());
}

#[test]
fn rejects_duplicate_permission() {
    let mut value = base();
    value["logic"] = json!({
        "api": 1,
        "module": "logic.wasm",
        "permissions": ["candidate-text", "candidate-text"]
    });
    assert!(parse(value).is_err());
}

#[test]
fn rejects_unknown_permission() {
    let mut value = base();
    value["logic"] = json!({
        "api": 1,
        "module": "logic.wasm",
        "permissions": ["root"]
    });
    assert!(parse(value).is_err());
}

#[test]
fn rejects_legacy_capabilities() {
    let mut value = legacy();
    value["capabilities"] = json!(["network"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_oversized_manifest() {
    let text = "x".repeat(MAX_MANIFEST_BYTES + 1);
    assert!(Manifest::parse(text.as_bytes()).is_err());
}
