use super::*;
use crate::config::OcrBackend;
use crate::settings_schema::PageId;
use lueying_extmeta::metrics::{ExtMetrics, ExtStat, ExtState};
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
    assert_eq!(
        settings.activate(&Id::new(ids::EXT_METRICS, 0)),
        Some(Action::RefreshExtMetrics)
    );
}

#[test]
fn ext_metrics_detail_summarizes_runtime() {
    let mut settings = Settings::default();
    assert!(settings.ext_metrics_detail().contains("暂无数据"));
    let stat = ExtStat {
        id: "shot".to_string(),
        name: "截图".to_string(),
        state: ExtState::Loaded,
        step_peak_us: 1500,
        ..ExtStat::default()
    };
    settings.ext_metrics = Some(ExtMetrics {
        extensions: vec![stat],
        ..ExtMetrics::default()
    });
    let detail = settings.ext_metrics_detail();
    assert!(detail.contains("运行 1"));
    assert!(detail.contains("步进峰值 1.5 ms"));
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
fn search_spans_pages_and_clears_on_navigation() {
    let mut settings = Settings::default();
    settings.set_search("辅助码");
    assert!(settings.searching());
    let page = settings.page(&crate::settings_view::PageColors::default());
    assert_eq!(page.title, "搜索");
    assert!(page.active_nav == usize::MAX);
    assert!(
        page.groups
            .iter()
            .flat_map(|group| &group.rows)
            .any(|row| row.label == "辅助码")
    );
    settings.set_page(PageId::Candidate);
    assert!(!settings.searching());
    assert_eq!(
        settings
            .page(&crate::settings_view::PageColors::default())
            .active_nav,
        1
    );
}

#[test]
fn candidate_page_starts_with_preview() {
    let settings = Settings::default();
    let page = settings.page_view(
        PageId::Candidate,
        &crate::settings_view::PageColors::default(),
    );
    let first = &page.groups[0].rows[0];
    assert_eq!(first.label, "候选窗预览");
    assert!(matches!(
        first.control,
        lueying_render::ui::Control::Preview { .. }
    ));
}

#[test]
fn app_override_buttons_cycle_three_states() {
    let mut settings = Settings::default();
    settings.cfg.apps.insert(
        "code.exe".to_string(),
        crate::config::AppOverride::default(),
    );
    let punct = Id::new(ids::app_punct(0), 0);
    let learning = Id::new(ids::app_learning(0), 0);
    settings.activate(&punct);
    assert_eq!(settings.cfg.apps["code.exe"].punct, Some(true));
    settings.activate(&punct);
    assert_eq!(settings.cfg.apps["code.exe"].punct, Some(false));
    settings.activate(&punct);
    assert_eq!(settings.cfg.apps["code.exe"].punct, None);
    settings.activate(&learning);
    assert_eq!(settings.cfg.apps["code.exe"].learning, Some(true));
    assert_eq!(
        settings.activate(&Id::new(ids::APPS_ADD, 0)),
        Some(Action::AddApp)
    );
}

#[test]
fn secret_extension_field_opens_editor() {
    use lueying_extmeta::Setting;
    let mut script = ScriptEntry::new("demo", "演示");
    script.settings = vec![Setting {
        key: "token".to_string(),
        title: "访问令牌".to_string(),
        detail: "只保存在本机".to_string(),
        kind: "secureText".to_string(),
        default: None,
        options: Vec::new(),
    }];
    let mut settings = Settings {
        scripts: vec![script],
        ..Settings::default()
    };
    settings.cfg.extensions = vec!["demo".to_string()];
    settings.set_page(PageId::Extensions);
    assert_eq!(
        settings.activate(&Id::new(ids::ext_field(0), 0)),
        Some(Action::EditSecret(
            "demo".to_string(),
            "token".to_string(),
            "访问令牌".to_string()
        ))
    );
}

#[test]
fn focus_order_follows_extension_group() {
    let mut script = ext_entry();
    // 待授权的包才有关闭不了的「同意」按钮,Tab 次序里才有它。
    script.consent = ConsentState::Missing;
    let mut settings = Settings {
        scripts: vec![script],
        ..Settings::default()
    };
    settings.cfg.extensions = vec!["shot".to_string()];
    settings.set_page(PageId::Extensions);
    let order = settings.focus_ids();
    let at = |base: &str| order.iter().position(|id| id.base == base).unwrap();
    assert!(at(ids::SCRIPTS) < at(ids::SCRIPTS_DIR));
    assert!(at(ids::SCRIPTS_DIR) < at(ids::CONSENT));
    assert!(at(ids::CONSENT) < at(ids::AUDIT));
    assert!(at(ids::AUDIT) < at(ids::EXT_METRICS));
    assert!(at(ids::EXT_METRICS) < at(ids::OCR));
    assert!(at(ids::OCR) < at(ids::OCR_DIR));
    assert!(at(ids::OCR_DIR) < at(ids::SAVE));
}

#[test]
fn focus_order_lists_script_buttons_once() {
    let mut settings = Settings::default();
    settings.set_page(PageId::Extensions);
    let order = settings.focus_ids();
    let choose = order.iter().filter(|id| id.base == ids::SCRIPTS).count();
    let dir = order
        .iter()
        .filter(|id| id.base == ids::SCRIPTS_DIR)
        .count();
    assert_eq!((choose, dir), (1, 1));
}

fn ext_entry() -> ScriptEntry {
    use lueying_extmeta::{Setting, SettingValue};
    let mut script = ScriptEntry::new("shot", "截图");
    script.settings = vec![
        Setting {
            key: "notify".to_string(),
            title: "完成后通知".to_string(),
            detail: String::new(),
            kind: "toggle".to_string(),
            default: Some(SettingValue::Bool(true)),
            options: Vec::new(),
        },
        Setting {
            key: "quality".to_string(),
            title: "识别质量".to_string(),
            detail: "慢一点更准".to_string(),
            kind: "choice".to_string(),
            default: Some(SettingValue::Text("high".to_string())),
            options: vec!["fast".to_string(), "high".to_string()],
        },
    ];
    script
}

#[test]
fn extension_settings_render_and_activate() {
    let mut settings = Settings {
        scripts: vec![ext_entry()],
        ..Settings::default()
    };
    settings.cfg.extensions = vec!["shot".to_string()];
    settings.set_page(PageId::Extensions);
    let toggle = ids::ext_field(0);
    let choice = ids::ext_field(1);
    assert!(Settings::is_segment(toggle));
    assert_eq!(settings.selected(choice), 1);

    assert_eq!(settings.activate(&Id::new(toggle, 0)), None);
    assert_eq!(
        settings.cfg.ext_settings["shot"]["notify"],
        serde_json::json!(false)
    );
    settings.activate(&Id::new(choice, 0));
    assert_eq!(
        settings.cfg.ext_settings["shot"]["quality"],
        serde_json::json!("fast")
    );
    assert!(settings.shift(&Id::new(choice, 0), 1));
    assert_eq!(
        settings.cfg.ext_settings["shot"]["quality"],
        serde_json::json!("high"),
        "左右键在选项里绕回"
    );
    assert!(!settings.shift(&Id::new(toggle, 0), 1), "开关不吃左右键");

    let focus = settings.focus_ids();
    let at = |base: &str| focus.iter().position(|id| id.base == base).unwrap();
    assert!(at(toggle) < at(choice));
    assert!(at(choice) < at(ids::SAVE), "扩展字段排在扩展页控件之后");
}
