use super::*;
use std::path::PathBuf;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lueying-config-unit-{}-{name}", std::process::id()))
}

fn load_text(name: &str, text: &str) -> AppConfig {
    let path = temp(name);
    std::fs::write(&path, text).expect("写临时配置");
    let cfg = load(&path);
    let _ = std::fs::remove_file(&path);
    cfg
}

#[test]
fn script_key_accepts_valid_id() {
    let cfg = load_text("valid", "script=my-pack_1\n");
    assert_eq!(cfg.script, "my-pack_1");
}

#[test]
fn script_key_off_values_disable() {
    for (index, value) in ["", "off", "none"].into_iter().enumerate() {
        let cfg = load_text(&format!("off-{index}"), &format!("script={value}\n"));
        assert!(cfg.script.is_empty(), "{value:?}");
    }
}

#[test]
fn script_key_rejects_paths_and_bad_ids() {
    let long = "a".repeat(49);
    let bad = [
        "../evil",
        "My-Pack",
        "-pack",
        "_pack",
        "pack/child",
        "pack\\child",
        long.as_str(),
    ];
    for (index, value) in bad.into_iter().enumerate() {
        let cfg = load_text(&format!("bad-{index}"), &format!("script={value}\n"));
        assert!(cfg.script.is_empty(), "{value:?}");
    }
}

#[test]
fn script_key_accepts_extension_alias() {
    let cfg = load_text("alias", "extension=demo-pack\n");
    assert_eq!(cfg.script, "demo-pack");
}

#[test]
fn valid_script_id_boundaries() {
    assert!(valid_script_id("a"));
    assert!(valid_script_id("0"));
    assert!(valid_script_id("a-b_c9"));
    assert!(valid_script_id(&"a".repeat(48)));
    assert!(!valid_script_id(""));
    assert!(!valid_script_id("A"));
    assert!(!valid_script_id("-a"));
    assert!(!valid_script_id("_a"));
    assert!(!valid_script_id("a b"));
    assert!(!valid_script_id(&"a".repeat(49)));
}

#[test]
fn save_keeps_script_and_round_trips() {
    let path = temp("roundtrip");
    let mut cfg = AppConfig::default();
    cfg.script = "demo-pack".to_string();
    save(&path, &cfg).expect("写配置");
    let text = std::fs::read_to_string(&path).expect("读配置");
    let loaded = load(&path);
    let _ = std::fs::remove_file(&path);
    assert!(text.contains("script=demo-pack\n"), "{text}");
    assert_eq!(loaded.script, "demo-pack");
}
