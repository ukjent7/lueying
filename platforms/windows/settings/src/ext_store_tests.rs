use super::*;
use serde_json::{Value, json};

fn package(raw: &[u8]) -> Vec<u8> {
    let manifest = Manifest::parse(raw).unwrap();
    container::encode(&vec![
        ("manifest.json".into(), raw.to_vec()),
        (manifest.payload_file().into(), vec![]),
    ])
    .unwrap()
}

#[test]
fn each_card_previews_its_checked_in_manifest() {
    let manifests = [
        include_bytes!("../../../../extensions/shot-ocr/manifest.json").as_slice(),
        include_bytes!("../../../../extensions/voice-asr/manifest.json").as_slice(),
        include_bytes!("../../../../extensions/clipboard-history/manifest.json").as_slice(),
    ];
    assert_eq!(OFFICIAL.len(), manifests.len());
    for raw in manifests {
        let manifest = Manifest::parse(raw).unwrap();
        let item = find(&manifest.id).unwrap();
        let items = item.permission_preview().unwrap();
        assert!(!items.is_empty());
        assert_eq!(items, consent::consent_items(&manifest));
        assert_eq!(item.version, manifest.version);
        let suffix = format!("/{0}-v{1}/{0}.luext", item.id, item.version);
        assert!(item.url().ends_with(&suffix));
        let bytes = package(raw);
        for card in OFFICIAL {
            assert_eq!(card.verify(&bytes).is_ok(), card.id == item.id);
        }
    }
}

#[test]
fn rejects_unlisted_native_and_wrong_version_metadata() {
    let item = find("shot-ocr").unwrap();
    for id in ["local-addon", "ppocr", "sensevoice"] {
        assert!(find(id).is_none());
    }
    let other = find("voice-asr").unwrap();
    let native = include_bytes!("../../../../extensions/ppocr/manifest.json");
    for bad_card in [
        Official {
            manifest_json: other.manifest_json,
            ..item
        },
        Official {
            id: "ppocr",
            manifest_json: native,
            ..item
        },
    ] {
        assert!(bad_card.permission_preview().is_err());
    }
    // 即使清单声称是同 id 同版本，原生包也不能冒充商店里的 Wasm。
    let mut native: Value = serde_json::from_slice(native).unwrap();
    native["id"] = json!(item.id);
    native["version"] = json!(item.version);
    let mut other_version: Value = serde_json::from_slice(item.manifest_json).unwrap();
    other_version["version"] = json!("0.0.0");
    for changed in [native, other_version] {
        let raw = serde_json::to_vec(&changed).unwrap();
        assert!(item.verify(&package(&raw)).is_err());
    }
}

#[test]
fn rejects_permission_changes_even_if_the_consent_text_is_identical() {
    let item = find("clipboard-history").unwrap();
    let preview = item.permission_preview().unwrap();
    for field in ["capabilities", "needs"] {
        let mut changed: Value = serde_json::from_slice(item.manifest_json).unwrap();
        changed[field].as_array_mut().unwrap().remove(0);
        let raw = serde_json::to_vec(&changed).unwrap();
        let manifest = Manifest::parse(&raw).unwrap();
        assert_eq!(consent::consent_items(&manifest), preview);
        assert!(item.verify(&package(&raw)).is_err(), "{field}");
    }
}
