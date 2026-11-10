//! 辅助码字形表:行首行尾空白、注释与坏行不影响其余条目。

use super::{AuxCode, AuxLookup};

#[test]
fn parses_entries_and_skips_bad_lines() {
    let table =
        AuxCode::from_text("# 字=码\n  你 = wq,ww \n\t好=vb\n坏行\n两字=ab\n你=ab,AB,toolong\n");
    assert_eq!(table.len(), 2, "只收单字");
    assert!(table.matches("你", "w"));
    assert!(table.matches("你", "wq"));
    assert!(table.matches("你", "ab"), "码统一小写,超长码丢弃");
    assert!(table.matches("好", "v"));
    assert!(!table.matches("好", "w"));
}
