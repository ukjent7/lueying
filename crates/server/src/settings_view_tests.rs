use super::*;
use crate::config::OcrBackend;
use lueying_render::ui::Id;

fn entry(id: &str, name: &str) -> ScriptEntry {
    ScriptEntry::new(id, name)
}

#[test]
fn select_script_sets_and_clears() {
    let mut settings = Settings {
        scripts: vec![entry("first", "第一个"), entry("second", "第二个")],
        ..Settings::default()
    };
    settings.select_script(Some(1));
    assert_eq!(settings.cfg.extensions, vec!["second"]);
    settings.select_script(None);
    assert!(settings.cfg.extensions.is_empty());
}

#[test]
fn select_script_out_of_range_closes() {
    let mut settings = Settings {
        scripts: vec![entry("only", "唯一")],
        ..Settings::default()
    };
    settings.cfg.extensions = vec!["only".to_string()];
    settings.select_script(Some(9));
    assert!(settings.cfg.extensions.is_empty());
}

#[test]
fn script_detail_reports_state() {
    let mut settings = Settings {
        scripts: vec![entry("demo", "演示包"), entry("other", "另一个")],
        ..Settings::default()
    };
    assert!(settings.script_detail().contains("未启用"));
    settings.cfg.extensions = vec!["demo".to_string()];
    let active = settings.script_detail();
    assert!(active.contains("演示包") && active.contains("demo"));
    settings.cfg.extensions = vec!["gone".to_string()];
    assert!(settings.script_detail().contains("未安装"));
    settings.cfg.extensions = vec!["demo".to_string(), "other".to_string()];
    let multiple = settings.script_detail();
    assert!(multiple.contains("已启用 2 个") && multiple.contains("demo, other"));
}

#[test]
fn script_buttons_map_to_actions() {
    let mut settings = Settings::default();
    assert_eq!(
        settings.activate(&Id::new(ids::SCRIPTS, 0)),
        Some(Action::ChooseScript)
    );
    assert_eq!(
        settings.activate(&Id::new(ids::SCRIPTS_DIR, 0)),
        Some(Action::OpenScripts)
    );
    assert_eq!(
        settings.activate(&Id::new(ids::CONSENT, 0)),
        Some(Action::ApproveExtension)
    );
    assert_eq!(
        settings.activate(&Id::new(ids::AUDIT, 0)),
        Some(Action::OpenExtAudit)
    );
}

#[test]
fn consent_detail_reports_pending_and_sensitive() {
    let mut pending = ScriptEntry::new("shot", "截图");
    pending.items = vec!["截取屏幕(高敏感)".to_string()];
    pending.consent = ConsentState::Missing;
    pending.sensitive = true;
    let mut settings = Settings {
        scripts: vec![pending],
        ..Settings::default()
    };
    let detail = |settings: &Settings| {
        crate::settings_consent::detail(&settings.cfg.extensions, &settings.scripts)
    };
    let enabled = |settings: &Settings| {
        crate::settings_consent::approve_enabled(&settings.cfg.extensions, &settings.scripts)
    };
    assert!(detail(&settings).contains("选择运行期脚本"));
    settings.cfg.extensions = vec!["shot".to_string()];
    let pending_detail = detail(&settings);
    assert!(pending_detail.contains("敏感组合"));
    assert!(pending_detail.contains("待同意"));
    assert!(pending_detail.contains("截取屏幕"));
    assert!(enabled(&settings));

    settings.scripts[0].consent = ConsentState::Changed {
        added: vec!["访问网络(外发)".to_string()],
    };
    let changed = detail(&settings);
    assert!(changed.contains("新增") && changed.contains("访问网络"));

    settings.scripts[0].consent = ConsentState::Approved;
    assert!(!enabled(&settings));
    assert!(detail(&settings).contains("已同意"));
}

#[test]
fn ocr_segment_sets_backend() {
    let mut settings = Settings::default();
    assert_eq!(settings.selected(ids::OCR), 0);
    settings.activate(&Id::new(ids::OCR, 1));
    assert_eq!(settings.cfg.ocr_backend, OcrBackend::Ppocr);
    assert_eq!(settings.selected(ids::OCR), 1);
    settings.activate(&Id::new(ids::OCR, 0));
    assert_eq!(settings.cfg.ocr_backend, OcrBackend::System);
}

#[test]
fn ocr_detail_reports_model_availability() {
    let mut settings = Settings::default();
    assert!(settings.ocr_detail().contains("未安装"));
    settings.ppocr_ready = true;
    assert!(settings.ocr_detail().contains("PP-OCRv6"));
    settings.cfg.ocr_backend = OcrBackend::Ppocr;
    assert!(settings.ocr_detail().contains("首次识别"));
}

#[test]
fn ocr_dir_maps_to_action() {
    let mut settings = Settings::default();
    assert_eq!(
        settings.activate(&Id::new(ids::OCR_DIR, 0)),
        Some(Action::OpenOcrModels)
    );
}

#[test]
fn focus_order_follows_extension_group() {
    let order = Settings::focus_order();
    let at = |base: &str| order.iter().position(|id| id.base == base).unwrap();
    assert!(at(ids::FUZZY) < at(ids::SCRIPTS));
    assert!(at(ids::SCRIPTS_DIR) < at(ids::CONSENT));
    assert!(at(ids::CONSENT) < at(ids::AUDIT));
    assert!(at(ids::AUDIT) < at(ids::OCR));
    assert!(at(ids::OCR) < at(ids::OCR_DIR));
    assert!(at(ids::OCR_DIR) < at(ids::PHRASES));
}

#[test]
fn focus_order_lists_script_buttons_once() {
    let order = Settings::focus_order();
    let choose = order.iter().filter(|id| id.base == ids::SCRIPTS).count();
    let dir = order
        .iter()
        .filter(|id| id.base == ids::SCRIPTS_DIR)
        .count();
    assert_eq!((choose, dir), (1, 1));
}
