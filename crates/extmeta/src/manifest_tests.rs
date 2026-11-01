use super::*;
use serde_json::json;

fn base() -> serde_json::Value {
    json!({
        "id": "my-pack",
        "name": "我的扩展",
        "version": "1.0.0",
        "api": 3,
        "wasm": "extension.wasm"
    })
}

fn shot_ocr() -> serde_json::Value {
    json!({
        "id": "shot-ocr",
        "name": "截图 OCR",
        "version": "0.1.0",
        "api": 3,
        "wasm": "extension.wasm",
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
fn parses_wasm_manifest() {
    let manifest = parse(shot_ocr()).expect("API 3 清单应通过");
    assert_eq!(manifest.wasm_main(), "extension.wasm");
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
fn allows_no_activation() {
    let manifest = parse(base()).expect("纯外观包应通过");
    assert!(manifest.commands().is_empty());
}

#[test]
fn parses_appearance_pack() {
    let mut value = base();
    value["appearance"] = json!(true);
    let manifest = parse(value).expect("外观包应通过");
    assert!(manifest.is_appearance());
    assert!(manifest.commands().is_empty());
}

#[test]
fn rejects_non_bool_appearance() {
    let mut value = base();
    value["appearance"] = json!({ "api": 1 });
    assert!(parse(value).is_err());
}

#[test]
fn rejects_old_api() {
    for api in [1, 2, 4] {
        let mut value = base();
        value["api"] = json!(api);
        assert!(parse(value).is_err(), "api={api}");
    }
}

#[test]
fn rejects_old_components() {
    for field in ["main", "logic", "panel"] {
        let mut value = base();
        value[field] = json!({ "api": 1 });
        assert!(parse(value).is_err(), "{field}");
    }
}

#[test]
fn rejects_missing_wasm() {
    let mut value = base();
    value.as_object_mut().expect("对象").remove("wasm");
    assert!(parse(value).is_err());
}

#[test]
fn rejects_bad_command() {
    for (id, title) in [("", "标题"), ("Run", "标题"), ("run", ""), ("run", "a\nb")] {
        let mut value = shot_ocr();
        value["activate"]["commands"] = json!([{ "id": id, "title": title }]);
        assert!(parse(value).is_err(), "{id:?} {title:?}");
    }
}

#[test]
fn rejects_duplicate_command() {
    let mut value = shot_ocr();
    value["activate"]["commands"] = json!([
        { "id": "run", "title": "一" },
        { "id": "run", "title": "二" }
    ]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_bad_trigger() {
    for trigger in ["", "a b", "a\nb", "x".repeat(33).as_str()] {
        let mut value = shot_ocr();
        value["activate"]["triggers"] = json!([trigger]);
        assert!(parse(value).is_err(), "{trigger:?}");
    }
}

#[test]
fn rejects_duplicate_trigger() {
    let mut value = shot_ocr();
    value["activate"]["triggers"] = json!(["ocr", "ocr"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_empty_activation() {
    let mut value = shot_ocr();
    value["activate"] = json!({});
    assert!(parse(value).is_err());
}

#[test]
fn rejects_unknown_service() {
    let mut value = shot_ocr();
    value["needs"] = json!(["capture", "telepathy"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_duplicate_service() {
    let mut value = shot_ocr();
    value["needs"] = json!(["capture", "capture"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_unknown_capability() {
    let mut value = shot_ocr();
    value["capabilities"] = json!(["root"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_duplicate_capability() {
    let mut value = shot_ocr();
    value["capabilities"] = json!(["ocr", "ocr"]);
    assert!(parse(value).is_err());
}

#[test]
fn rejects_bad_wasm_entry() {
    for wasm in [
        "../x.wasm",
        "a/b.wasm",
        ".hidden.wasm",
        "x.js",
        "x.wasm/",
        "x.WASM",
    ] {
        let mut value = base();
        value["wasm"] = json!(wasm);
        assert!(parse(value).is_err(), "{wasm:?}");
    }
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
fn parses_network_whitelist() {
    let mut value = shot_ocr();
    value["capabilities"] = json!(["screen-capture", "network"]);
    value["needs"] = json!(["capture", "net"]);
    value["network"] = json!({ "domains": ["api.example.com", "localhost:11434"] });
    let manifest = parse(value).expect("网络白名单应通过");
    let domains = &manifest.network.expect("network").domains;
    assert_eq!(domains, &["api.example.com", "localhost:11434"]);
}

#[test]
fn rejects_network_domain_with_path() {
    let mut value = shot_ocr();
    value["network"] = json!({ "domains": ["https://example.com/api"] });
    assert!(parse(value).is_err());
}

#[test]
fn rejects_network_domain_with_zero_port() {
    let mut value = shot_ocr();
    value["network"] = json!({ "domains": ["localhost:0"] });
    assert!(parse(value).is_err());
}

#[test]
fn rejects_oversized_manifest() {
    let text = "x".repeat(MAX_MANIFEST_BYTES + 1);
    assert!(Manifest::parse(text.as_bytes()).is_err());
}

#[test]
fn parses_declarative_settings() {
    let mut value = shot_ocr();
    value["settings"] = json!([
        {
            "key": "notify",
            "title": "完成后通知",
            "detail": "识别完成弹系统通知",
            "type": "toggle",
            "default": true
        },
        {
            "key": "quality",
            "title": "识别质量",
            "type": "choice",
            "options": ["fast", "high"],
            "default": "high"
        }
    ]);
    let manifest = parse(value).expect("声明式设置应通过");
    let notify = &manifest.settings[0];
    assert!(notify.is_toggle());
    assert_eq!(
        notify.resolve(None),
        SettingValue::Bool(true),
        "缺省用清单默认值"
    );
    assert_eq!(
        notify.resolve(Some(&json!(false))),
        SettingValue::Bool(false)
    );
    assert_eq!(
        notify.resolve(Some(&json!("yes"))),
        SettingValue::Bool(true),
        "类型不符退回默认值"
    );
    let quality = &manifest.settings[1];
    assert_eq!(quality.default_value(), SettingValue::Text("high".into()));
    assert_eq!(
        quality.resolve(Some(&json!("ultra"))),
        SettingValue::Text("high".into()),
        "选项外的值退回默认值"
    );
    assert!(quality.accepts(&json!("fast")));
    assert!(!quality.accepts(&json!("ultra")));
    assert!(!quality.accepts(&json!(1)));
}

#[test]
fn parses_secure_text_setting() {
    let mut value = shot_ocr();
    value["settings"] = json!([
        {
            "key": "api_key",
            "title": "API 密钥",
            "detail": "只保存在本机",
            "type": "secureText"
        }
    ]);
    let manifest = parse(value).expect("密钥设置应通过");
    let setting = &manifest.settings[0];
    assert!(setting.is_secret());
    assert_eq!(setting.default_value(), SettingValue::Text(String::new()));
    assert!(setting.accepts(&json!("sk-test")));
    assert!(!setting.accepts(&json!(true)));
    assert!(!setting.accepts(&json!("line\nbreak")));
}

#[test]
fn rejects_bad_settings() {
    let value = shot_ocr();
    let check = |setting: serde_json::Value| {
        let mut value = value.clone();
        value["settings"] = json!([setting]);
        assert!(parse(value).is_err());
    };
    check(json!({"key": "Bad Key", "title": "x", "type": "toggle"}));
    check(json!({"key": "ok", "title": "", "type": "toggle"}));
    check(json!({"key": "ok", "title": "x", "type": "switch"}));
    check(json!({"key": "ok", "title": "x", "type": "toggle", "options": ["a"]}));
    check(json!({"key": "ok", "title": "x", "type": "choice", "options": []}));
    check(json!({"key": "ok", "title": "x", "type": "choice", "options": ["a"], "default": "b"}));
    check(json!({"key": "ok", "title": "x", "type": "choice", "options": ["a", "a"]}));
    check(json!({"key": "ok", "title": "x", "type": "toggle", "extra": 1}));
    let mut value = shot_ocr();
    value["settings"] = json!([
        {"key": "same", "title": "一", "type": "toggle"},
        {"key": "same", "title": "二", "type": "toggle"}
    ]);
    assert!(parse(value).is_err());
}
