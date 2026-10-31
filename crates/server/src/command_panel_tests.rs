//! 命令面板页面模型与动作映射的回归测试。

use super::*;

fn dict() -> Arc<Dictionary> {
    let (dict, _) = Dictionary::from_text(include_str!("../../../dicts/mini.txt"));
    Arc::new(dict)
}

fn items() -> Vec<CommandItem> {
    vec![
        CommandItem {
            id: "a.ocr".into(),
            title: "OCR 取字".into(),
            extension: "demo".into(),
        },
        CommandItem {
            id: "b.xuexi".into(),
            title: "学习".into(),
            extension: "demo".into(),
        },
        CommandItem {
            id: "c.gongzuo".into(),
            title: "工作".into(),
            extension: "demo".into(),
        },
        CommandItem {
            id: "d.dazi".into(),
            title: "打字".into(),
            extension: "demo".into(),
        },
    ]
}

fn typed(text: &str) -> CommandPanel {
    let mut panel = CommandPanel::new(dict(), items());
    for ch in text.chars() {
        panel.type_char(ch);
    }
    panel
}

#[test]
fn empty_query_lists_every_command() {
    let mut panel = CommandPanel::new(dict(), items());
    let view = panel.view(8);
    assert_eq!(view.total, 4);
    assert_eq!(view.rows.len(), 4);
    assert_eq!(view.selected, 0);
    assert_eq!(view.rows[0].title, "OCR 取字");
    assert_eq!(view.rows[0].extension, "demo");
}

#[test]
fn pinyin_matches_command_title() {
    let panel = typed("xuexi");
    assert_eq!(
        panel.selected_item().map(|item| item.id.as_str()),
        Some("b.xuexi")
    );
    assert_eq!(panel.matches(), 1);
}

#[test]
fn abbreviation_and_full_pinyin_both_work() {
    assert_eq!(
        typed("gongzuo")
            .selected_item()
            .map(|item| item.id.as_str()),
        Some("c.gongzuo")
    );
    // 简拼交给引擎;迷你词库覆盖 gz → 工作。
    assert_eq!(
        typed("gz").selected_item().map(|item| item.id.as_str()),
        Some("c.gongzuo")
    );
}

#[test]
fn latin_title_matches_directly() {
    let panel = typed("ocr");
    assert_eq!(
        panel.selected_item().map(|item| item.id.as_str()),
        Some("a.ocr")
    );
}

#[test]
fn backspace_restores_previous_matches() {
    let mut panel = typed("xuexi");
    assert_eq!(panel.matches(), 1);
    for _ in 0..5 {
        assert!(panel.backspace());
    }
    assert_eq!(panel.query(), "");
    assert_eq!(panel.matches(), 4);
    assert!(!panel.backspace(), "空查询不能再删");
}

#[test]
fn non_letters_only_affect_direct_match() {
    let mut panel = typed("ocr");
    panel.type_char('-');
    assert_eq!(panel.query(), "ocr-");
    assert_eq!(panel.matches(), 0);
    panel.backspace();
    assert_eq!(panel.matches(), 1);
}

#[test]
fn selection_moves_and_clamps() {
    let mut panel = CommandPanel::new(dict(), items());
    assert!(!panel.move_by(-1));
    assert!(panel.move_by(1));
    assert_eq!(
        panel.selected_item().map(|item| item.id.as_str()),
        Some("b.xuexi")
    );
    assert!(panel.move_by(10));
    assert_eq!(
        panel.selected_item().map(|item| item.id.as_str()),
        Some("d.dazi")
    );
    assert!(!panel.move_by(1));
}

#[test]
fn view_keeps_selection_visible() {
    let items: Vec<CommandItem> = (0..12)
        .map(|i| CommandItem {
            id: format!("e.cmd{i}"),
            title: format!("A{i}"),
            extension: "demo".into(),
        })
        .collect();
    let mut panel = CommandPanel::new(dict(), items);
    for ch in "a".chars() {
        panel.type_char(ch);
    }
    assert_eq!(panel.matches(), 12);
    let view = panel.view(5);
    assert_eq!((view.above, view.below), (0, 7));
    panel.move_by(6);
    let view = panel.view(5);
    assert_eq!((view.above, view.below), (2, 5));
    assert_eq!(view.selected, 4);
    assert_eq!(view.rows[4].title, "A6");
}

#[test]
fn query_cap_ignores_extra_letters() {
    let mut panel = CommandPanel::new(dict(), items());
    for _ in 0..MAX_QUERY {
        assert!(panel.type_char('a'));
    }
    assert!(!panel.type_char('a'));
    assert_eq!(panel.query().chars().count(), MAX_QUERY);
}

#[test]
fn reset_clears_query_and_selection() {
    let mut panel = typed("xuexi");
    panel.move_by(0);
    panel.reset();
    assert_eq!(panel.query(), "");
    assert_eq!(panel.matches(), 4);
    assert_eq!(
        panel.selected_item().map(|item| item.id.as_str()),
        Some("a.ocr")
    );
}
