use super::*;
use lueying_extmeta::SettingValue;

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
fn fields_resolve_defaults_and_stored_values() {
    let scripts = vec![script()];
    let extensions = vec!["shot-ocr".to_string()];
    let cfg = AppConfig::default();
    let fields = fields(&extensions, &scripts, &cfg);
    assert_eq!(fields.len(), 2);
    assert_eq!(fields[0].slot, 0);
    assert!(matches!(fields[0].kind, Kind::Toggle { on: true }));
    assert_eq!(
        fields[1].kind,
        Kind::Choice {
            options: vec!["fast".to_string(), "high".to_string()],
            selected: 1
        }
    );
    assert_eq!(fields[1].detail, "慢一点更准");

    let mut cfg = AppConfig::default();
    cfg.ext_settings
        .entry("shot-ocr".to_string())
        .or_default()
        .insert("quality".to_string(), Value::String("fast".to_string()));
    let fields = fields(&extensions, &scripts, &cfg);
    assert_eq!(fields[1].selected(), 0);
}

#[test]
fn apply_writes_toggle_and_choice() {
    let scripts = vec![script()];
    let cfg = AppConfig::default();
    let fields = fields(&["shot-ocr".to_string()], &scripts, &cfg);
    let mut cfg = cfg;
    assert!(apply(&mut cfg, &fields[0], 0));
    assert_eq!(
        cfg.ext_settings["shot-ocr"]["notify"],
        Value::Bool(false),
        "开关点击是翻转"
    );
    assert!(apply(&mut cfg, &fields[1], 0));
    assert_eq!(
        cfg.ext_settings["shot-ocr"]["quality"],
        Value::String("fast".to_string())
    );
    assert!(!apply(&mut cfg, &fields[1], 9), "越界选项不写");
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
fn values_merge_defaults() {
    let stored = BTreeMap::from([("notify".to_string(), Value::Bool(false))]);
    let value = values(&[toggle(), choice()], Some(&stored));
    assert_eq!(
        value,
        serde_json::json!({"notify": false, "quality": "high"})
    );
}

#[test]
fn slots_match_static_prefixes() {
    assert_eq!(slot("ext-0"), Some(0));
    assert_eq!(slot("ext-15"), Some(15));
    assert_eq!(slot("ext-16"), None);
    assert_eq!(slot("ocr"), None);
}
