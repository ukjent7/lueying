use super::*;
use crate::settings::model::{Control, Id};
use crate::settings::schema::PageId;
use lueying_ext::manifest::metrics::{ExtMetrics, ExtStat, ExtState};
use lueying_server::config::OcrBackend;

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
        settings.activate(&Id::new(ids::IMPORT_LUEXT, 0)),
        Some(Action::ImportLuext)
    );
    assert_eq!(
        settings.activate(&Id::new(ids::CONSENT, 0)),
        Some(Action::ApproveExtension)
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
        dispatch_peak_us: 1500,
        ..ExtStat::default()
    };
    settings.ext_metrics = Some(ExtMetrics {
        extensions: vec![stat],
        ..ExtMetrics::default()
    });
    let detail = settings.ext_metrics_detail();
    assert!(detail.contains("运行 1"));
    assert!(detail.contains("派发峰值 1.5 ms"));
}

#[test]
fn consent_detail_reports_pending_and_changed() {
    let mut pending = ScriptEntry::new("shot", "截图");
    pending.items = vec!["截取屏幕(高敏感)".to_string()];
    pending.consent = ConsentState::Missing;
    let mut settings = Settings {
        scripts: vec![pending],
        ..Settings::default()
    };
    let detail = |settings: &Settings| {
        crate::settings::consent::detail(&settings.cfg.extensions, &settings.scripts)
    };
    let enabled = |settings: &Settings| {
        crate::settings::consent::approve_enabled(&settings.cfg.extensions, &settings.scripts)
    };
    assert!(detail(&settings).contains("选择运行期扩展"));
    settings.cfg.extensions = vec!["shot".to_string()];
    let pending_detail = detail(&settings);
    assert!(pending_detail.contains("待同意"));
    assert!(pending_detail.contains("截取屏幕"));
    assert!(enabled(&settings));

    settings.scripts[0].consent = ConsentState::Changed {
        added: vec!["读写剪贴板(高敏感)".to_string()],
    };
    let changed = detail(&settings);
    assert!(changed.contains("新增") && changed.contains("剪贴板"));

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
    let page = settings.page(&crate::settings::view::PageColors::default());
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
            .page(&crate::settings::view::PageColors::default())
            .active_nav,
        1
    );
}

#[test]
fn candidate_page_starts_with_preview() {
    let settings = Settings::default();
    let page = settings.page_view(
        PageId::Candidate,
        &crate::settings::view::PageColors::default(),
    );
    let first = &page.groups[0].rows[0];
    assert_eq!(first.label, "候选窗预览");
    assert!(matches!(
        first.control,
        crate::settings::model::Control::Preview { .. }
    ));
}

#[test]
fn app_override_buttons_cycle_three_states() {
    let mut settings = Settings::default();
    settings.cfg.apps.insert(
        "code.exe".to_string(),
        lueying_server::config::AppOverride::default(),
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
    use lueying_ext::manifest::{Setting, SettingValue};
    let mut script = ScriptEntry::new("demo", "演示");
    script.settings = vec![Setting {
        key: "token".to_string(),
        title: "访问令牌".to_string(),
        detail: "只保存在本机".to_string(),
        kind: "secureText".to_string(),
        default: Some(SettingValue::Text("unused-default".into())),
        options: Vec::new(),
    }];
    let mut settings = Settings {
        scripts: vec![script],
        ..Settings::default()
    };
    let groups = settings.extension_settings("demo");
    assert!(!format!("{groups:?}").contains("unused-default"));
    let Control::Buttons { items } = &groups[0].rows[0].control else {
        panic!("应显示密钥编辑按钮");
    };
    assert_eq!(
        settings.activate(&items[0].id),
        Some(Action::EditSecret(
            "demo".to_string(),
            "token".to_string(),
            "访问令牌".to_string()
        ))
    );
    assert!(settings.cfg.ext_settings.is_empty());
    assert!(settings.cfg.extensions.is_empty());
}

fn ext_entry() -> ScriptEntry {
    use lueying_ext::manifest::{Setting, SettingValue};
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
    let groups = settings.extension_settings("shot");
    let rows = &groups[0].rows;
    let Control::Toggle { id: toggle, .. } = &rows[0].control else {
        panic!("通知应为开关");
    };
    let Control::Segment { id: choice, .. } = &rows[1].control else {
        panic!("识别质量应为单选");
    };
    assert!(Settings::is_segment(&choice.base));
    assert_eq!(settings.selected(&choice.base), 1);

    assert_eq!(settings.activate(toggle), None);
    assert_eq!(
        settings.cfg.ext_settings["shot"]["notify"],
        serde_json::json!(false)
    );
    settings.activate(choice);
    assert_eq!(
        settings.cfg.ext_settings["shot"]["quality"],
        serde_json::json!("fast")
    );
    assert!(settings.shift(choice, 1));
    assert_eq!(
        settings.cfg.ext_settings["shot"]["quality"],
        serde_json::json!("high"),
        "左右键在选项里绕回"
    );
    assert!(!settings.shift(toggle, 1), "开关不吃左右键");
}

#[test]
fn installed_cards_keep_all_fields_and_edit_only_their_own_keys() {
    use lueying_ext::manifest::{EXT_API, Manifest};
    use serde_json::json;

    fn installed(id: &str) -> ScriptEntry {
        let fields: Vec<_> = (0..32)
            .map(|n| {
                json!({
                    "key": format!("mode-{n}"), "title": format!("{id} {n}"),
                    "type": "choice", "options": ["auto", "manual"]
                })
            })
            .collect();
        let manifest = json!({
            "api": EXT_API, "id": id, "name": id, "version": "1.0.0",
            "wasm": "extension.wasm", "settings": fields,
        });
        let manifest = Manifest::parse(manifest.to_string().as_bytes()).unwrap();
        let mut script = ScriptEntry::new(manifest.id, manifest.name);
        script.settings = manifest.settings;
        script
    }

    let mut settings = Settings {
        scripts: vec![installed("first"), installed("second")],
        ..Settings::default()
    };
    settings.cfg.extensions = vec!["first".into(), "second".into()];
    let last = |settings: &Settings, id: &str| {
        let groups = settings.extension_settings(id);
        let [group] = groups.as_slice() else {
            panic!("{id} 应有一组设置");
        };
        assert_eq!(group.rows.len(), 32);
        assert_eq!(group.rows[31].label, format!("{id} 31"));
        let Control::Segment { id, .. } = &group.rows[31].control else {
            panic!("模式应为单选");
        };
        id.clone()
    };
    let first = last(&settings, "first");
    let second = last(&settings, "second");
    assert_ne!(first, second);

    settings.cfg.extensions.pop();
    settings.scripts[1].consent = ConsentState::Missing;
    assert_eq!(last(&settings, "second"), second);
    settings.scripts.reverse();
    settings.scripts[0].settings.reverse();
    assert_eq!(settings.activate(&Id::new(second.base.clone(), 1)), None);
    assert_eq!(settings.selected(&first.base), 0);
    assert_eq!(settings.selected(&second.base), 1);
    assert_eq!(
        settings.cfg.ext_settings["second"]["mode-31"],
        json!("manual")
    );
    assert!(settings.shift(&second, 1));
    let values = &settings.cfg.ext_settings;
    assert_eq!(values.len(), 1);
    assert_eq!(values["second"]["mode-31"], json!("auto"));
    assert_eq!(settings.cfg.extensions, ["first"]);
    assert_eq!(settings.scripts[0].consent, ConsentState::Missing);

    // 卸载后收到旧卡片的点击,不能退到另一张卡片的同名字段。
    let before = values.clone();
    settings.scripts.remove(0);
    assert!(settings.extension_settings("second").is_empty());
    assert_eq!(settings.activate(&second), None);
    assert!(!settings.shift(&second, 1));
    assert_eq!(settings.cfg.ext_settings, before);
}

#[test]
fn enabling_same_category_displaces_the_old_one() {
    use lueying_ext::manifest::Category;
    let skin = |id: &str| {
        let mut script = entry(id, id);
        script.category = Some(Category::Skin);
        script
    };
    let mut settings = Settings {
        scripts: vec![skin("old"), skin("new"), entry("other", "其它")],
        ..Settings::default()
    };
    settings.cfg.extensions = vec!["old".to_string(), "other".to_string()];
    settings.activate(&Id::new(ids::script_toggle(1), 0));
    assert_eq!(settings.cfg.extensions, vec!["other", "new"]);
    assert!(
        settings.message.contains("已停用同类扩展:old"),
        "提示要点名被停用的扩展: {}",
        settings.message
    );
    // 关掉当前扩展不算启用,不触发互斥,也不留提示。
    settings.activate(&Id::new(ids::script_toggle(1), 0));
    assert_eq!(settings.cfg.extensions, vec!["other"]);
}

#[test]
fn uncategorised_extensions_never_displace_each_other() {
    let mut settings = Settings {
        scripts: vec![entry("first", "第一"), entry("second", "第二")],
        ..Settings::default()
    };
    settings.cfg.extensions = vec!["first".to_string()];
    settings.activate(&Id::new(ids::script_toggle(1), 0));
    assert_eq!(settings.cfg.extensions, vec!["first", "second"]);
}

#[test]
fn store_is_opt_in_and_only_opens_the_selected_extension() {
    let mut settings = Settings::default();
    settings.set_page(PageId::Extensions);
    let page = settings.page(&PageColors::default());
    let rows: Vec<_> = page.groups.iter().flat_map(|group| &group.rows).collect();
    assert_eq!(
        rows.iter()
            .filter(|row| row.detail.contains("可选下载"))
            .count(),
        3
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.label == "识别模型" || row.detail.contains("PP-OCR"))
    );
    assert_eq!(
        settings.activate(&Id::new(ids::EXT_STORE, 0)),
        Some(Action::OpenExtension("shot-ocr".into()))
    );
    assert!(settings.cfg.extensions.is_empty());

    let mut shot = ext_entry();
    shot.id = "shot-ocr".into();
    settings.scripts = vec![shot, entry("local", "本地扩展")];
    settings.set_extension_enabled("shot-ocr", true);
    assert!(!settings.extension_settings("shot-ocr").is_empty());
    assert!(settings.extension_settings("local").is_empty());
    let page = settings.page(&PageColors::default());
    assert!(
        page.groups
            .iter()
            .flat_map(|group| &group.rows)
            .any(|row| { row.label == "截图识别" && row.detail.starts_with("已启用") })
    );
    assert_eq!(
        settings.activate(&Id::new(ids::EXT_INSTALLED, 1)),
        Some(Action::OpenExtension("local".into()))
    );
}
