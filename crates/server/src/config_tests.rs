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
    assert_eq!(cfg.extensions, vec!["my-pack_1"]);
}

#[test]
fn script_key_off_values_disable() {
    for (index, value) in ["", "off", "none"].into_iter().enumerate() {
        let cfg = load_text(&format!("off-{index}"), &format!("script={value}\n"));
        assert!(cfg.extensions.is_empty(), "{value:?}");
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
        assert!(cfg.extensions.is_empty(), "{value:?}");
    }
}

#[test]
fn script_key_accepts_extension_alias() {
    let cfg = load_text("alias", "extension=demo-pack\n");
    assert_eq!(cfg.extensions, vec!["demo-pack"]);
}

#[test]
fn extensions_key_keeps_order_and_skips_bad_items() {
    let cfg = load_text(
        "list",
        "extensions=shot-ocr, clipboard-history,../evil,shot-ocr,,\n",
    );
    assert_eq!(cfg.extensions, vec!["shot-ocr", "clipboard-history"]);
}

#[test]
fn bindings_key_parses_pairs_and_skips_bad_items() {
    let cfg = load_text(
        "bind",
        "bindings=Ctrl+Alt+O=shot.run, bad-item,, Ctrl+Shift+L=clip.search ,Ctrl+Alt+O=dup,=empty,x=\n",
    );
    assert_eq!(
        cfg.bindings,
        vec![
            ("Ctrl+Alt+O".to_string(), "shot.run".to_string()),
            ("Ctrl+Shift+L".to_string(), "clip.search".to_string()),
        ]
    );
}

#[test]
fn extensions_key_replaces_legacy_script() {
    let cfg = load_text("both", "script=old-pack\nextensions=new-pack\n");
    assert_eq!(cfg.extensions, vec!["new-pack"]);
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
fn save_keeps_extensions_and_round_trips() {
    let path = temp("roundtrip");
    let cfg = AppConfig {
        extensions: vec!["first".to_string(), "second".to_string()],
        ..AppConfig::default()
    };
    save(&path, &cfg).expect("写配置");
    let text = std::fs::read_to_string(&path).expect("读配置");
    let loaded = load(&path);
    let _ = std::fs::remove_file(&path);
    assert!(text.contains("extensions=first,second\n"), "{text}");
    assert_eq!(loaded.extensions, vec!["first", "second"]);
}
