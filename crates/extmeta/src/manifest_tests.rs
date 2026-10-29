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

fn script_v2() -> serde_json::Value {
    json!({
        "id": "shot-ocr",
        "name": "截图 OCR",
        "version": "0.1.0",
        "api": 2,
        "main": "index.js",
        "activate": {
            "triggers": ["ocr"],
            "hotkeys": ["Ctrl+Alt+O"],
            "commands": [
                { "id": "run", "title": "截图并识别文字", "when": "any" }
            ]
        },
        "needs": ["capture", "ocr", "output"],
        "capabilities": ["screen-capture", "ocr"]
    })
}

fn parse(value: serde_json::Value) -> Result<Manifest, String> {
    Manifest::parse(value.to_string().as_bytes())
}

#[test]
fn parses_script_v2() {
    let manifest = parse(script_v2()).expect("API 2 清单应通过");
    assert_eq!(manifest.script_main(), Some("index.js"));
    assert_eq!(
        manifest.needs,
        vec![Service::Capture, Service::Ocr, Service::Output]
    );
    assert_eq!(
        manifest.commands(),
        &[Command {
            id: "run".into(),
            title: "截图并识别文字".into(),
            when: When::Any,
        }]
    );
    let activate = manifest.activate().expect("激活声明");
    assert_eq!(activate.triggers, vec!["ocr"]);
    assert_eq!(activate.hotkeys, vec!["Ctrl+Alt+O"]);
}

#[test]
fn v2_defaults_entry_and_allows_no_activation() {
    let value = json!({
        "id": "skin",
        "name": "外观",
        "version": "1.0.0",
        "api": 2
    });
    let manifest = parse(value).expect("API 2 外观包应通过");
    assert_eq!(manifest.script_main(), Some("index.js"));
    assert!(manifest.commands().is_empty());
}

#[test]
fn rejects_v2_mixed_with_components() {
    let mut value = script_v2();
    value["appearance"] = json!({ "api": 1 });
    assert!(parse(value).is_err());
}

#[test]
fn rejects_v2_bad_command() {
    for (id, title) in [("", "标题"), ("Run", "标题"), ("run", ""), ("run", "a\nb")] {
        let mut value = script_v2();
        value["activate"]["commands"] = json!([{ "id": id, "title": title }]);
        assert!(parse(value).is_err(), "{id:?} {title:?}");
    }
}

#[test]
fn rejects_v2_duplicate_command() {
    let mut value = script_v2();
    value["activate"]["commands"] = json!([
        { "id": "run", "title": "一" },
        { "id": "run", "title": "二" }
    ]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_v2_bad_trigger() {
    for trigger in ["", "a b", "a\nb", "x".repeat(33).as_str()] {
        let mut value = script_v2();
        value["activate"]["triggers"] = json!([trigger]);
        assert!(parse(value).is_err(), "{trigger:?}");
    }
}

#[test]
fn rejects_v2_duplicate_trigger() {
    let mut value = script_v2();
    value["activate"]["triggers"] = json!(["ocr", "ocr"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_v2_empty_activation() {
    let mut value = script_v2();
    value["activate"] = json!({});
    assert!(parse(value).is_err());
}

#[test]
fn rejects_v2_unknown_service() {
    let mut value = script_v2();
    value["needs"] = json!(["capture", "telepathy"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_v2_duplicate_service() {
    let mut value = script_v2();
    value["needs"] = json!(["capture", "capture"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_v2_unknown_capability() {
    let mut value = script_v2();
    value["capabilities"] = json!(["root"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_v2_duplicate_capability() {
    let mut value = script_v2();
    value["capabilities"] = json!(["ocr", "ocr"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_v2_bad_entry() {
    for main in ["../x.js", "a/b.js", ".hidden.js", "x.txt"] {
        let mut value = script_v2();
        value["main"] = json!(main);
        assert!(parse(value).is_err(), "{main:?}");
    }
}

#[test]
fn rejects_v1_activation() {
    let mut value = legacy();
    value["activate"] = json!({ "commands": [{ "id": "run", "title": "跑" }] });
    assert!(parse(value).is_err());
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
