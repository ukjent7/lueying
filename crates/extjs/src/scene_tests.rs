use super::*;

#[test]
fn parses_accent_and_effects() {
    let patch = ScenePatch::parse(
        r#"{"accent":2048,"effects":{"background":"half4 main(float2 p) { return half4(1.0); }"}}"#,
    )
    .expect("补丁应通过");
    assert_eq!(patch.accent, Some(2048));
    let effects = patch.effects.expect("有槽位");
    assert_eq!(
        effects.background.as_deref(),
        Some("half4 main(float2 p) { return half4(1.0); }")
    );
}

#[test]
fn rejects_unknown_fields() {
    assert!(ScenePatch::parse(r#"{"accent":1,"extra":true}"#).is_err());
    assert!(ScenePatch::parse(r#"{"effects":{"shadow":"x"}}"#).is_err());
}

#[test]
fn rejects_accent_over_24_bits() {
    assert!(ScenePatch::parse(r#"{"accent":16777216}"#).is_err());
    assert!(ScenePatch::parse(r#"{"accent":16777215}"#).is_ok());
}

#[test]
fn rejects_empty_and_nul_effect_source() {
    assert!(ScenePatch::parse(r#"{"effects":{"menu":"  "}}"#).is_err());
    assert!(ScenePatch::parse(r#"{"effects":{"menu":"a\u0000b"}}"#).is_err());
}

#[test]
fn rejects_oversized_effect_source() {
    let source = "a".repeat(MAX_EFFECT_BYTES + 1);
    let text = serde_json::json!({"effects": {"background": source}}).to_string();
    assert!(ScenePatch::parse(&text).is_err());
}

#[test]
fn rejects_oversized_patch() {
    let text = "x".repeat(MAX_PATCH_BYTES + 1);
    assert!(ScenePatch::parse(&text).is_err());
}

#[test]
fn iterates_present_effects_in_slot_order() {
    let patch =
        ScenePatch::parse(r#"{"effects":{"menu":"m","background":"b"}}"#).expect("补丁应通过");
    let effects = patch.effects.expect("有槽位");
    let slots: Vec<(EffectSlot, &str)> = effects.iter().collect();
    assert_eq!(
        slots,
        vec![(EffectSlot::Background, "b"), (EffectSlot::Menu, "m")]
    );
}
