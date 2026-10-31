//! 宿主选择面板页面模型的回归测试。

use super::*;

fn panel(items: &[&str]) -> HostPanel {
    let args = serde_json::json!({ "title": "选择语言", "items": items }).to_string();
    HostPanel::parse(&args).unwrap()
}

#[test]
fn parses_and_trims_title_and_items() {
    let panel = HostPanel::parse(r#"{"title":" 选择语言 ","items":[" 中文 ","English"]}"#).unwrap();
    assert_eq!(panel.title(), "选择语言");
    let view = panel.clone().view(8);
    assert_eq!(view.rows, vec!["中文".to_string(), "English".to_string()]);
    assert_eq!(view.selected, 0);
    assert_eq!(view.total, 2);
}

#[test]
fn rejects_empty_or_oversized_fields() {
    assert!(HostPanel::parse(r#"{"title":"","items":["甲"]}"#).is_err());
    assert!(HostPanel::parse(r#"{"title":"板","items":[]}"#).is_err());
    assert!(HostPanel::parse(r#"{"title":"板","items":[""]}"#).is_err());
    assert!(HostPanel::parse(r#"{"title":"板","items":[1]}"#).is_err());
    let too_many = serde_json::json!({
        "title": "板",
        "items": vec!["甲"; MAX_ITEMS + 1],
    })
    .to_string();
    assert!(HostPanel::parse(&too_many).is_err());
    let long = "很".repeat(MAX_ITEM + 1);
    let too_long = serde_json::json!({ "title": "板", "items": [long] }).to_string();
    assert!(HostPanel::parse(&too_long).is_err());
}

#[test]
fn view_scrolls_to_selection() {
    let mut panel = panel(&[
        "一", "二", "三", "四", "五", "六", "七", "八", "九", "十", "十一", "十二",
    ]);
    assert!(panel.move_by(9));
    let view = panel.view(4);
    assert_eq!(view.selected, 3);
    assert_eq!(
        view.rows,
        vec![
            "七".to_string(),
            "八".to_string(),
            "九".to_string(),
            "十".to_string()
        ]
    );
    assert_eq!(view.above, 6);
    assert_eq!(view.below, 2);
    assert_eq!(panel.selected_index(), 9);

    assert!(panel.move_by(isize::MAX / 2));
    let view = panel.view(4);
    assert_eq!(panel.selected_index(), 11);
    assert_eq!(view.above, 8);
    assert_eq!(view.below, 0);

    assert!(!panel.move_by(1));
    assert!(panel.move_by(-100));
    let view = panel.view(4);
    assert_eq!(panel.selected_index(), 0);
    assert_eq!(view.above, 0);
}
