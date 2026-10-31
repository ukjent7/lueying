//! 设置 schema 与运行时配置的绑定测试。

use super::*;
use crate::accents::ACCENTS;
use crate::config::AppConfig;

/// 有界面的设置,默认值必须与 `AppConfig::default` 一致。
#[test]
fn defaults_match_runtime_config() {
    let cfg = AppConfig::default();
    for def in SETTINGS {
        let Some(ui) = &def.ui else {
            continue;
        };
        match (&ui.control, def.default) {
            (ControlDef::Toggle { get, .. }, DefaultValue::Bool(expected)) => {
                assert_eq!(get(&cfg), expected, "{} 默认值不一致", def.key);
            }
            (ControlDef::Segment { get, choices, .. }, DefaultValue::Int(expected)) => {
                let actual = choices
                    .get(get(&cfg))
                    .and_then(|choice| choice.value.parse::<i64>().ok());
                assert_eq!(actual, Some(expected), "{} 默认值不一致", def.key);
            }
            (ControlDef::Segment { get, choices, .. }, DefaultValue::Text(expected)) => {
                let selected = choices.get(get(&cfg)).map(|choice| choice.value);
                assert_eq!(selected, Some(expected), "{} 默认值不一致", def.key);
            }
            (_, default) => panic!("{} 的默认值类型不匹配: {default:?}", def.key),
        }
    }
}

/// 配置键与控件 id 各只出现一次。
#[test]
fn keys_and_controls_are_unique() {
    for (index, def) in SETTINGS.iter().enumerate() {
        assert!(
            SETTINGS[..index].iter().all(|other| other.key != def.key),
            "配置键 {} 重复",
            def.key
        );
        if let Some(ui) = &def.ui {
            let id = ui.control.id();
            assert!(
                SETTINGS[..index]
                    .iter()
                    .filter_map(|other| other.ui.as_ref())
                    .all(|other| other.control.id() != id),
                "控件 id {id} 重复"
            );
        }
    }
}

/// 选项表的配置值与名称必须和运行时枚举一一对应。
#[test]
fn choices_match_runtime_enums() {
    for (choice, scheme) in SCHEME_CHOICES.iter().zip(SchemeId::ALL) {
        assert_eq!(choice.value, scheme.as_str());
        assert_eq!(choice.label, scheme.name());
    }
    assert_eq!(SCHEME_CHOICES.len(), SchemeId::ALL.len());

    for (choice, name) in REGION_CHOICES.iter().zip(Region::NAMES) {
        assert_eq!(choice.label, name);
    }
    for (choice, name) in FONT_CHOICES.iter().zip(UiFont::NAMES) {
        assert_eq!(choice.label, name);
    }
    for (choice, name) in LAYOUT_CHOICES.iter().zip(UiLayout::NAMES) {
        assert_eq!(choice.label, name);
    }
    for (choice, name) in OCR_CHOICES.iter().zip(OcrBackend::NAMES) {
        assert_eq!(choice.label, name);
    }
    for (choice, name) in CHANNEL_CHOICES.iter().zip(Channel::NAMES) {
        assert_eq!(choice.label, name);
    }
    for (choice, accent) in ACCENT_CHOICES.iter().zip(ACCENTS.iter()) {
        assert_eq!(choice.label, accent.name);
    }
}

/// JSON Schema 覆盖每个配置键,并保留默认值与控件类型。
#[test]
fn schema_covers_every_key() {
    let schema = config_schema_json();
    let properties = schema["properties"]
        .as_object()
        .expect("schema properties 是对象");
    assert_eq!(properties.len(), SETTINGS.len());
    for def in SETTINGS {
        let property = properties.get(def.key).expect("配置键在 schema 里");
        assert!(property.get("default").is_some(), "{} 缺默认值", def.key);
        match def.value {
            ValueType::Bool => assert_eq!(property["type"], "boolean"),
            ValueType::Int { .. } => assert_eq!(property["type"], "integer"),
            ValueType::Enum(_) => assert_eq!(property["type"], "string"),
            ValueType::Text => assert_eq!(property["type"], "string"),
            ValueType::List => assert_eq!(property["type"], "array"),
            ValueType::Table => assert_eq!(property["type"], "object"),
        }
    }
    let text = config_schema_text();
    assert!(text.contains("\"page_size\"") && text.contains("\"traditional_region\""));
}

/// 每个有配置项的页面至少有一行;词库页全是动作行,允许为空。
#[test]
fn pages_have_settings() {
    for page in PageId::ALL {
        let count = on_page(page).count();
        if page == PageId::Dictionary {
            assert_eq!(count, 0);
        } else {
            assert!(count > 0, "{} 页没有设置项", page.title());
        }
    }
}
