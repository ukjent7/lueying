use super::*;
use std::path::PathBuf;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lueying-config-unit-{}-{name}", std::process::id()))
}

fn load_text(_name: &str, text: &str) -> AppConfig {
    // 旧 INI 用例直接走迁移解析器;正式入口只读 config.toml。
    let mut merged = AppConfig::default();
    let mut issues = Vec::new();
    crate::config_file::apply(
        &mut merged,
        crate::config_file::from_ini(text),
        "config.ini",
        &mut issues,
    );
    merged
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
fn triggers_key_normalizes_and_skips_bad_items() {
    let cfg = load_text(
        "trigger",
        "triggers=BC=shot.run, sj=date.now ,bc=dup,中文=x,=empty,x=\n",
    );
    assert_eq!(
        cfg.triggers,
        vec![
            ("bc".to_string(), "shot.run".to_string()),
            ("sj".to_string(), "date.now".to_string()),
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
fn panel_hotkey_parses_and_off_values_disable() {
    let cfg = load_text("panel", "panel_hotkey=Ctrl+Alt+Shift+P\n");
    assert_eq!(cfg.panel_hotkey, "Ctrl+Alt+Shift+P");
    for (index, value) in ["", "off", "none"].into_iter().enumerate() {
        let cfg = load_text(
            &format!("panel-off-{index}"),
            &format!("panel_hotkey={value}\n"),
        );
        assert!(cfg.panel_hotkey.is_empty(), "{value:?}");
    }
}

#[test]
fn panel_hotkey_defaults_to_three_modifiers() {
    let cfg = load_text("panel-default", "");
    assert_eq!(cfg.panel_hotkey, "Ctrl+Alt+Shift+Space");
}

#[test]
fn ocr_engine_parses_aliases() {
    assert_eq!(load_text("ocr-default", "").ocr_backend, OcrBackend::System);
    for value in ["ppocr", "pp-ocr", "paddle"] {
        let cfg = load_text(&format!("ocr-{value}"), &format!("ocr_engine={value}\n"));
        assert_eq!(cfg.ocr_backend, OcrBackend::Ppocr, "{value}");
    }
    assert_eq!(
        load_text("ocr-alias", "ocr=ppocr\n").ocr_backend,
        OcrBackend::Ppocr
    );
    assert_eq!(
        load_text("ocr-unknown", "ocr_engine=magic\n").ocr_backend,
        OcrBackend::System
    );
}

#[test]
fn ppocr_dir_requires_all_three_files() {
    let dir = temp("ppocr-dir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    assert!(!ppocr_complete(&dir));
    std::fs::write(dir.join("det.onnx"), b"x").unwrap();
    std::fs::write(dir.join("rec.onnx"), b"x").unwrap();
    assert!(!ppocr_complete(&dir));
    std::fs::write(dir.join("dict.txt"), b"x").unwrap();
    assert!(ppocr_complete(&dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_keeps_extensions_and_round_trips() {
    let path = temp("roundtrip.toml");
    let cfg = AppConfig {
        extensions: vec!["first".to_string(), "second".to_string()],
        triggers: vec![("bc".to_string(), "shot.run".to_string())],
        panel_hotkey: "Ctrl+Alt+Shift+P".to_string(),
        ocr_backend: OcrBackend::Ppocr,
        ..AppConfig::default()
    };
    save(&path, &cfg).expect("写配置");
    let text = std::fs::read_to_string(&path).expect("读配置");
    let loaded = load(&path);
    let _ = std::fs::remove_file(&path);
    assert!(text.contains("extensions"), "{text}");
    assert!(
        text.contains("\"first\"") && text.contains("\"second\""),
        "{text}"
    );
    assert!(text.contains("bc = \"shot.run\""), "{text}");
    assert!(
        text.contains("panel_hotkey = \"Ctrl+Alt+Shift+P\""),
        "{text}"
    );
    assert!(text.contains("ocr_engine = \"ppocr\""), "{text}");
    assert_eq!(
        loaded.triggers,
        vec![("bc".to_string(), "shot.run".to_string())]
    );
    assert_eq!(loaded.extensions, vec!["first", "second"]);
    assert_eq!(loaded.panel_hotkey, "Ctrl+Alt+Shift+P");
    assert_eq!(loaded.ocr_backend, OcrBackend::Ppocr);
}

#[test]
fn toml_schema_merges_core_apps_and_extension_namespace() {
    let text = r#"
page_size = 9
ui_accent = "青"
extensions = ["shot-ocr", "../evil", "shot-ocr"]
[bindings]
"Ctrl+Alt+O" = "shot.run"
[triggers]
BC = "shot.run"
[ext.shot-ocr]
model = "ppocr"
limit = 3
"#;
    let mut cfg = AppConfig::default();
    let mut issues = Vec::new();
    let file: crate::config_file::FileConfig = toml::from_str(text).expect("解析 TOML");
    crate::config_file::apply(&mut cfg, file, "config.toml", &mut issues);
    assert_eq!(cfg.page_size, 9);
    assert_eq!(cfg.ui_accent, 1);
    assert_eq!(cfg.extensions, vec!["shot-ocr"]);
    assert_eq!(
        cfg.bindings,
        vec![("Ctrl+Alt+O".to_string(), "shot.run".to_string())]
    );
    assert_eq!(
        cfg.triggers,
        vec![("bc".to_string(), "shot.run".to_string())]
    );
    assert_eq!(
        cfg.ext_settings["shot-ocr"]["model"],
        serde_json::Value::String("ppocr".into())
    );
    assert!(issues.iter().any(|issue| issue.message.contains("../evil")));
}

#[test]
fn toml_schema_rejects_unknown_keys() {
    assert!(toml::from_str::<crate::config_file::FileConfig>("nope = 1").is_err());
    assert!(toml::from_str::<crate::config_file::FileConfig>("page_size = \"big\"").is_err());
}

#[test]
fn apps_toml_lowercases_hosts_and_skips_nodes() {
    let text = r#"
["NotePad.exe"]
punct = false
learning = true
["other.exe"]
learning = false
"#;
    let layers = crate::config_file::apps_from_toml(text).expect("解析 apps.toml");
    let config = AppConfig {
        apps: layers
            .into_iter()
            .map(|(host, layer)| (host, layer.into()))
            .collect(),
        ..AppConfig::default()
    };
    assert_eq!(
        config.app_override("notepad.exe"),
        AppOverride {
            punct: Some(false),
            learning: Some(true),
        }
    );
}
