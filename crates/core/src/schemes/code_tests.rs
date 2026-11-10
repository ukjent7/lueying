//! 形码方案的码表:解析、前缀命中与候选顺序。

use std::sync::Arc;

use crate::{Config, Engine, Key};
use lueying_dict::Dictionary;

use super::{CodeScheme, CodeTable};

const TABLE: &str = "\
# 码\t字词\t权重
nk\t你\t1000
nk\t尼\t200
nka\t哪\t300
nh\t好\t900
";

fn table() -> CodeTable {
    CodeTable::from_text(TABLE)
}

#[test]
fn parses_entries_and_skips_bad_lines() {
    let table = table();
    assert_eq!(table.len(), 4);
    let with_bad = CodeTable::from_text("nk\t你\t1000\n坏行\n\t空码\t1\nnk\t\t2\n");
    assert_eq!(with_bad.len(), 1);
}

#[test]
fn prefix_lookup_ranks_exact_code_first() {
    let table = table();
    let hits = table.lookup("nk");
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].text, "你", "同一个码下权重大的排前面");
    assert!(hits[0].full);
    assert_eq!(hits[1].text, "尼");
    assert!(hits[1].full);
    assert_eq!(hits[2].text, "哪", "没敲全的码排在同码之后");
    assert!(!hits[2].full, "没敲全的码不算吃完整串");
}

#[test]
fn prefix_lookup_matches_codes_under_the_prefix() {
    let table = table();
    let hits = table.lookup("n");
    let texts: Vec<&str> = hits.iter().map(|hit| hit.text.as_str()).collect();
    assert_eq!(texts, vec!["你", "好", "哪", "尼"]);
}

#[test]
fn unknown_prefix_has_no_candidates() {
    assert!(table().lookup("z").is_empty());
    assert!(table().lookup("").is_empty());
}

#[test]
fn engine_commits_the_code_candidate() {
    let scheme = Arc::new(CodeScheme::new("demo-code", "演示码", table()));
    let mut engine =
        Engine::new(Arc::new(Dictionary::builtin()), Config::default()).with_scheme(scheme);
    for key in ['n', 'k'] {
        engine.process(Key::Letter(key));
    }
    let candidates = engine.candidates();
    assert_eq!(candidates.first().map(String::as_str), Some("你"));
    let events = engine.process(Key::Space);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, crate::Event::Commit(text) if text == "你")),
        "{events:?}"
    );
}
