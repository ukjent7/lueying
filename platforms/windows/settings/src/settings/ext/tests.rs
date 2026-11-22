use super::*;
use lueying_ext::manifest::SettingValue;
use std::collections::BTreeMap;

fn toggle() -> Setting {
    Setting {
        key: "notify".to_string(),
        title: "完成后通知".to_string(),
        detail: String::new(),
        kind: "toggle".to_string(),
        default: Some(SettingValue::Bool(true)),
        options: Vec::new(),
    }
}

fn choice() -> Setting {
    Setting {
        key: "quality".to_string(),
        title: "识别质量".to_string(),
        detail: "慢一点更准".to_string(),
        kind: "choice".to_string(),
        default: Some(SettingValue::Text("high".to_string())),
        options: vec!["fast".to_string(), "high".to_string()],
    }
}

fn script() -> ScriptEntry {
    let mut script = ScriptEntry::new("shot-ocr", "截图 OCR");
    script.settings = vec![toggle(), choice()];
    script
}

#[test]
fn rows_resolve_defaults_and_stored_values() {
    let script = script();
    let mut cfg = AppConfig::default();
    let rows = group(&script, &cfg).rows;
    assert_eq!(rows.len(), 2);
    assert!(matches!(rows[0].control, Control::Toggle { on: true, .. }));
    let Control::Segment {
        options, selected, ..
    } = &rows[1].control
    else {
        panic!("识别质量应为单选");
    };
    assert_eq!(
        options
            .iter()
            .map(|option| option.text.as_str())
            .collect::<Vec<_>>(),
        ["fast", "high"]
    );
    assert_eq!(*selected, 1);
    assert_eq!(rows[1].detail, "慢一点更准");

    cfg.ext_settings
        .entry("shot-ocr".to_string())
        .or_default()
        .insert("quality".to_string(), Value::String("fast".to_string()));
    assert!(matches!(
        group(&script, &cfg).rows[1].control,
        Control::Segment { selected: 0, .. }
    ));
}

#[test]
fn apply_writes_toggle_and_choice() {
    let scripts = [script()];
    let field = |key| Field::find(&scripts, &ids::ext_field("shot-ocr", key)).unwrap();
    let mut cfg = AppConfig::default();
    assert!(!field("quality").apply(&mut cfg, 9), "越界选项不写");
    assert!(cfg.ext_settings.is_empty());
    assert!(field("notify").apply(&mut cfg, 0));
    assert_eq!(
        cfg.ext_settings["shot-ocr"]["notify"],
        Value::Bool(false),
        "开关点击是翻转"
    );
    assert!(field("quality").apply(&mut cfg, 0));
    assert_eq!(
        cfg.ext_settings["shot-ocr"]["quality"],
        Value::String("fast".to_string())
    );
}

#[test]
fn normalize_drops_unknown_and_bad_values() {
    let scripts = vec![script()];
    let mut cfg = AppConfig::default();
    let table = cfg.ext_settings.entry("shot-ocr".to_string()).or_default();
    table.insert("notify".to_string(), Value::Bool(false));
    table.insert("quality".to_string(), Value::String("ultra".to_string()));
    table.insert("legacy".to_string(), Value::Bool(true));
    assert_eq!(normalize(&mut cfg, &scripts), 2);
    assert_eq!(
        cfg.ext_settings["shot-ocr"],
        BTreeMap::from([("notify".to_string(), Value::Bool(false))])
    );
}

#[test]
fn lookup_requires_an_installed_extension_and_its_declared_key() {
    let scripts = [script()];
    for base in ["ext-0", "ocr", "ext:other:notify", "ext:shot-ocr:missing"] {
        assert!(Field::find(&scripts, base).is_none());
    }
}
