use super::*;
use std::path::PathBuf;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lueying-config-unit-{}-{name}", std::process::id()))
}

fn load_text(_name: &str, text: &str) -> AppConfig {
    let mut merged = AppConfig::default();
    let mut issues = Vec::new();
    let file: crate::config_file::FileConfig = toml::from_str(text).expect("解析 TOML");
    crate::config_file::apply(&mut merged, file, "config.toml", &mut issues);
    merged
}

#[test]
fn extensions_key_accepts_valid_id() {
    let cfg = load_text("valid", "extensions = [\"my-pack_1\"]\n");
    assert_eq!(cfg.extensions, vec!["my-pack_1"]);
}

#[test]
fn extensions_key_off_values_disable() {
    let cfg = load_text("off", "extensions = []\n");
    assert!(cfg.extensions.is_empty());
}

#[test]
fn extensions_key_rejects_paths_and_bad_ids() {
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
        let cfg = load_text(
            &format!("bad-{index}"),
            &format!("extensions = [{}]\n", toml_string(value)),
        );
        assert!(cfg.extensions.is_empty(), "{value:?}");
    }
}

#[test]
fn extensions_key_keeps_order_and_skips_bad_items() {
    let cfg = load_text(
        "list",
        "extensions = [\"shot-ocr\", \"clipboard-history\", \"../evil\", \"shot-ocr\"]\n",
    );
    assert_eq!(cfg.extensions, vec!["shot-ocr", "clipboard-history"]);
}

#[test]
fn bindings_key_skips_empty_items() {
    let cfg = load_text(
        "bind",
        "[bindings]\n\"Ctrl+Alt+O\" = \"shot.run\"\n\"Ctrl+Shift+L\" = \"clip.search\"\n\"\" = \"empty\"\n",
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
        "[triggers]\nBC = \"shot.run\"\nSJ = \"date.now\"\n\"中文\" = \"x\"\n\"\" = \"empty\"\n",
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
fn ocr_engine_parses_aliases() {
    assert_eq!(load_text("ocr-default", "").ocr_backend, OcrBackend::System);
    for value in ["ppocr", "pp-ocr", "paddle"] {
        let cfg = load_text(
            &format!("ocr-{value}"),
            &format!("ocr_engine = {}\n", toml_string(value)),
        );
        assert_eq!(cfg.ocr_backend, OcrBackend::Ppocr, "{value}");
    }
    assert_eq!(
        load_text("ocr-unknown", "ocr_engine = \"magic\"\n").ocr_backend,
        OcrBackend::System
    );
}

#[test]
fn ppocr_dir_requires_all_five_files() {
    let dir = temp("ppocr-dir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建目录");
    let files = [
        "manifest.json",
        "ppocr.exe",
        "det.onnx",
        "rec.onnx",
        "dict.txt",
    ];
    for (index, name) in files.iter().enumerate() {
        std::fs::write(dir.join(name), b"x").unwrap();
        assert_eq!(ppocr_complete(&dir), index == files.len() - 1, "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_keeps_extensions_and_round_trips() {
    let path = temp("roundtrip.toml");
    let cfg = AppConfig {
        extensions: vec!["first".to_string(), "second".to_string()],
        triggers: vec![("bc".to_string(), "shot.run".to_string())],
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
    assert!(text.contains("ocr_engine = \"ppocr\""), "{text}");
    assert_eq!(
        loaded.triggers,
        vec![("bc".to_string(), "shot.run".to_string())]
    );
    assert_eq!(loaded.extensions, vec!["first", "second"]);
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

/// TOML 基本字符串转义,测试里嵌任意值用。
fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
