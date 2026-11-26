//! 跨上屏边界整词的保护与撤销。

use std::sync::Arc;

use lueying_dict::Dictionary;

use crate::{Config, Engine, Event, Key, session};

const WORDS: &str = "\
装逼\t1288\tzhuang'bi
大连\t33257\tda'lian
打脸\t11994\tda'lian
装逼打脸\t111\tzhuang'bi'da'lian
";

fn dictionary(extra: &str) -> Arc<Dictionary> {
    let (dict, bad) = Dictionary::from_text(&format!("{WORDS}{extra}"));
    assert_eq!(bad, 0);
    Arc::new(dict)
}

fn new_engine(extra: &str) -> Engine {
    Engine::new(dictionary(extra), Config::default())
}

fn type_into(engine: &mut Engine, raw: &str) {
    for c in raw.chars() {
        assert!(engine.process(Key::Letter(c)).is_empty());
    }
}

fn pick(engine: &mut Engine, text: &str, full: bool) {
    let index = engine
        .candidates_detailed()
        .iter()
        .position(|cand| cand.text == text && cand.full == full)
        .unwrap();
    assert_eq!(engine.pick(index), [Event::Commit(text.to_string())]);
}

fn commit_zhuangbi(engine: &mut Engine) {
    type_into(engine, "zhuangbi");
    pick(engine, "装逼", true);
}

fn type_dalian_with_context(engine: &mut Engine) {
    engine.set_context("他说要去装逼");
    type_into(engine, "dalian");
}

fn prepared(extra: &str) -> Engine {
    let mut engine = new_engine(extra);
    commit_zhuangbi(&mut engine);
    type_dalian_with_context(&mut engine);
    engine
}

fn index_of(engine: &Engine, text: &str) -> usize {
    engine
        .candidates_detailed()
        .iter()
        .position(|cand| cand.text == text && cand.full)
        .unwrap()
}

#[test]
fn unique_attested_compound_locks_the_verified_continuation() {
    let mut engine = prepared("");
    let candidates = engine.candidates();
    assert_eq!(candidates.first().map(String::as_str), Some("打脸"));
    assert!(candidates.iter().any(|text| text == "大连"));
    assert!(candidates.iter().any(|text| text == "打脸"));

    let snapshot = session::snapshot(&engine).expect("组合仍在");
    assert!(snapshot.facts[0].locked);

    let dalian = index_of(&engine, "大连");
    assert!(!engine.promote(dalian));
    assert!(!engine.reorder_candidates(&[dalian, 0]));
    assert_eq!(
        engine.candidates().first().map(String::as_str),
        Some("打脸")
    );
    assert_eq!(
        engine.process(Key::Space),
        [Event::Commit("打脸".to_string())]
    );
}

#[test]
fn context_mismatch_or_punctuation_releases_the_protection() {
    let mut engine = prepared("");
    assert_eq!(engine.candidates()[0], "打脸");
    engine.set_context("他说要去");
    assert_eq!(engine.candidates()[0], "大连");
    let snapshot = session::snapshot(&engine).expect("组合仍在");
    assert!(!snapshot.facts[0].locked);

    let mut engine = new_engine("");
    commit_zhuangbi(&mut engine);
    assert_eq!(
        engine.process(Key::Punct(',')),
        [Event::Commit("，".to_string())]
    );
    type_into(&mut engine, "dalian");
    assert_eq!(engine.candidates()[0], "大连");
    let snapshot = session::snapshot(&engine).expect("组合仍在");
    assert!(!snapshot.facts[0].locked);
    assert_eq!(snapshot.adjacent_word.as_deref(), None);
}

#[test]
fn snapshot_exposes_only_the_still_adjacent_committed_word() {
    let mut engine = prepared("");
    let snapshot = session::snapshot(&engine).expect("组合仍在");
    assert_eq!(snapshot.adjacent_word.as_deref(), Some("装逼"));
    engine.set_context("他说要去装比");
    let snapshot = session::snapshot(&engine).expect("组合仍在");
    assert_eq!(snapshot.adjacent_word.as_deref(), None);
}

#[test]
fn two_attested_compounds_keep_the_original_order() {
    let engine = prepared("装逼大连\t50\tzhuang'bi'da'lian\n");
    assert_eq!(engine.candidates()[0], "大连");
    let snapshot = session::snapshot(&engine).expect("组合仍在");
    assert!(!snapshot.facts.iter().any(|fact| fact.locked));
}

#[test]
fn manual_pick_still_commits_the_unpromoted_reading() {
    let mut engine = prepared("");
    assert_eq!(engine.candidates()[0], "打脸");
    pick(&mut engine, "大连", true);
}

#[test]
fn user_pin_stays_above_the_attested_continuation() {
    let mut engine = prepared("");
    assert_eq!(engine.candidates()[0], "打脸");
    let dalian = index_of(&engine, "大连");
    assert!(engine.pin(dalian));
    assert_eq!(engine.candidates()[0], "大连");
    assert!(engine.candidates_detailed()[0].pinned);
}

#[test]
fn selecting_a_prefix_protects_the_remainder_immediately() {
    let mut engine = new_engine("");
    type_into(&mut engine, "zhuangbidalian");
    pick(&mut engine, "装逼", false);
    assert_eq!(engine.composition().unwrap().raw, "dalian");
    assert_eq!(engine.candidates()[0], "打脸");
    assert!(session::snapshot(&engine).unwrap().facts[0].locked);
    assert_eq!(
        engine.process(Key::Space),
        [Event::Commit("打脸".to_string())]
    );
}

#[test]
fn traditional_display_context_keeps_the_dictionary_anchor() {
    let mut engine = new_engine("").with_modules(Arc::new(crate::features::builtins(
        crate::Script::Traditional,
    )));
    type_into(&mut engine, "zhuangbi");
    pick(&mut engine, "裝逼", true);
    engine.set_context("他說要去裝逼");
    type_into(&mut engine, "dalian");
    assert_eq!(engine.candidates()[0], "打臉");
    let snapshot = session::snapshot(&engine).unwrap();
    assert!(snapshot.facts[0].locked);
    assert_eq!(snapshot.adjacent_word.as_deref(), Some("装逼"));
}

#[test]
fn learning_disabled_context_has_no_commit_anchor() {
    let mut engine = new_engine("");
    engine.set_learning(false);
    type_into(&mut engine, "zhuangbi");
    pick(&mut engine, "装逼", true);
    engine.set_context("他说要去装逼");
    type_into(&mut engine, "dalian");
    assert_eq!(engine.candidates()[0], "大连");
}
