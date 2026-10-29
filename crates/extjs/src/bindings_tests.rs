//! 绑定表回归:解析归一、语法拒绝、保留组合、表内冲突与 when 过滤。

use super::{BindingTable, Input, parse};
use crate::manifest::When;

#[test]
fn parse_normalizes_order_case_and_spelling() {
    let hotkey = parse("alt+CTRL+o").unwrap();
    assert_eq!(hotkey.canonical(), "Ctrl+Alt+O");
    let twin = parse("Ctrl + Alt + O").unwrap();
    assert_eq!(hotkey, twin);
    let named = parse("ctrl+shift+pageup").unwrap();
    assert_eq!(named.canonical(), "Ctrl+Shift+PageUp");
    let fkey = parse("shift+win+f4").unwrap();
    assert_eq!(fkey.canonical(), "Shift+Win+F4");
    assert_eq!(
        parse("ctrl+windows+space").err().unwrap(),
        "热键 \"Ctrl+Win+Space\" 是系统保留组合"
    );
}

#[test]
fn parse_rejects_grammar_violations() {
    assert!(parse("Ctrl+O").is_err(), "单修饰键要拒");
    assert!(parse("Ctrl+Alt+O+P").is_err(), "多主键要拒");
    assert!(parse("Ctrl+Alt+").is_err(), "空段要拒");
    assert!(parse("Ctrl+Alt").is_err(), "缺主键要拒");
    assert!(parse("Ctrl+Ctrl+O").is_err(), "重复修饰键要拒");
    assert!(parse("Ctrl+Shift+F13").is_err(), "F 键越界要拒");
    assert!(parse("Ctrl+Alt+你好").is_err(), "主键不合法要拒");
}

#[test]
fn parse_rejects_reserved_combos() {
    for combo in [
        "Ctrl+Alt+Del",
        "Ctrl+Shift+Esc",
        "Ctrl+Win+Space",
        "Ctrl+Win+Left",
        "Ctrl+Win+Right",
        "Ctrl+Win+D",
        "Ctrl+Win+F4",
    ] {
        assert!(parse(combo).is_err(), "{combo} 应该被保留");
    }
}

#[test]
fn insert_reports_conflict_with_existing_command() {
    let mut table = BindingTable::default();
    table
        .insert("Ctrl+Alt+O", "shot.run".into(), When::Any)
        .unwrap();
    let error = table
        .insert("alt+ctrl+o", "clip.search".into(), When::Any)
        .unwrap_err();
    assert!(error.contains("已绑定 shot.run"), "{error}");
    assert_eq!(table.rows().len(), 1, "冲突的绑定不进表");
}

#[test]
fn hit_filters_by_when() {
    let mut table = BindingTable::default();
    table
        .insert("Ctrl+Alt+C", "shot.run".into(), When::Composing)
        .unwrap();
    table
        .insert("Ctrl+Alt+E", "clip.search".into(), When::Editable)
        .unwrap();
    table
        .insert("Ctrl+Alt+A", "hot.help".into(), When::Any)
        .unwrap();
    let composing = Input {
        editable: true,
        composing: true,
    };
    let editing = Input {
        editable: true,
        composing: false,
    };
    let idle = Input {
        editable: false,
        composing: false,
    };
    assert!(table.hit("ctrl+alt+c", composing).is_some());
    assert!(table.hit("ctrl+alt+c", editing).is_none());
    assert!(table.hit("ctrl+alt+c", idle).is_none());
    assert!(table.hit("ctrl+alt+e", editing).is_some());
    assert!(table.hit("ctrl+alt+e", idle).is_none());
    assert!(table.hit("ctrl+alt+a", idle).is_some(), "any 处处生效");
    assert!(
        table.hit("ctrl+alt+z", composing).is_none(),
        "没绑的组合不命中"
    );
}
