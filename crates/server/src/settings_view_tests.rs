use super::*;
use lueying_render::ui::Id;

fn entry(id: &str, name: &str) -> ScriptEntry {
    ScriptEntry {
        id: id.to_string(),
        name: name.to_string(),
    }
}

#[test]
fn select_script_sets_and_clears() {
    let mut settings = Settings {
        scripts: vec![entry("first", "第一个"), entry("second", "第二个")],
        ..Settings::default()
    };
    settings.select_script(Some(1));
    assert_eq!(settings.cfg.script, "second");
    settings.select_script(None);
    assert!(settings.cfg.script.is_empty());
}

#[test]
fn select_script_out_of_range_closes() {
    let mut settings = Settings {
        scripts: vec![entry("only", "唯一")],
        ..Settings::default()
    };
    settings.cfg.script = "only".to_string();
    settings.select_script(Some(9));
    assert!(settings.cfg.script.is_empty());
}

#[test]
fn script_detail_reports_state() {
    let mut settings = Settings {
        scripts: vec![entry("demo", "演示包")],
        ..Settings::default()
    };
    assert!(settings.script_detail().contains("未启用"));
    settings.cfg.script = "demo".to_string();
    let active = settings.script_detail();
    assert!(active.contains("演示包") && active.contains("demo"));
    settings.cfg.script = "gone".to_string();
    assert!(settings.script_detail().contains("未安装"));
}

#[test]
fn script_buttons_map_to_actions() {
    let settings = Settings::default();
    assert_eq!(
        settings.activate(&Id::new(ids::SCRIPTS, 0)),
        Some(Action::ChooseScript)
    );
    assert_eq!(
        settings.activate(&Id::new(ids::SCRIPTS_DIR, 0)),
        Some(Action::OpenScripts)
    );
}

#[test]
fn focus_order_lists_script_buttons_once() {
    let order = Settings::focus_order();
    let choose = order.iter().filter(|id| id.base == ids::SCRIPTS).count();
    let dir = order.iter().filter(|id| id.base == ids::SCRIPTS_DIR).count();
    assert_eq!((choose, dir), (1, 1));
}
