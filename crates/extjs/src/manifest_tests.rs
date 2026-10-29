use super::*;
use serde_json::json;

fn base() -> serde_json::Value {
    json!({
        "id": "my-pack",
        "name": "我的候选窗",
        "version": "1.0.0",
        "api": 1
    })
}

fn parse(value: serde_json::Value) -> Result<Manifest, String> {
    Manifest::parse(value.to_string().as_bytes())
}

#[test]
fn parses_minimal_manifest() {
    let manifest = parse(base()).expect("最小清单应通过");
    assert_eq!(manifest.id, "my-pack");
    assert_eq!(manifest.main, "index.js");
    assert!(manifest.capabilities.is_empty());
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
fn rejects_api_out_of_range() {
    for api in [0, 2] {
        let mut value = base();
        value["api"] = json!(api);
        assert!(parse(value).is_err(), "api={api}");
    }
}

#[test]
fn rejects_capabilities() {
    let mut value = base();
    value["capabilities"] = json!(["network"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_bad_main() {
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
        value["main"] = json!(main);
        assert!(parse(value).is_err(), "{main:?}");
    }
}
