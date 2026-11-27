use std::sync::Arc;

use crate::{Config, Engine, Event, Key};
use lueying_dict::Dictionary;

fn engine() -> Engine {
    let (d, bad) = Dictionary::from_text(
        "你\t9000\tni\n拟\t600\tni\n好\t9000\thao\n你好\t5000\tni'hao\n拟好\t800\tni'hao\n\
         世\t3000\tshi\n界\t2500\tjie\n世界\t4000\tshi'jie\n视界\t400\tshi'jie\n\
         妮\t500\tni\n泥\t400\tni\n号\t3500\thao\n浩\t2000\thao\n毫\t1000\thao\n\
         吗\t7500\tma\n马\t3000\tma\n码\t1500\tma\n",
    );
    assert_eq!(bad, 0);
    Engine::new(Arc::new(d), Config::default())
}

fn type_into(e: &mut Engine, raw: &str) {
    for ch in raw.chars() {
        assert!(e.process(Key::Letter(ch)).is_empty());
    }
}

fn pick(e: &mut Engine, text: &str, full: bool) {
    let index = e
        .cands
        .iter()
        .position(|c| c.text == text && c.full == full)
        .unwrap();
    assert_eq!(e.pick(index), [Event::Commit(text.to_string())]);
}

fn shape(e: &Engine) -> (usize, usize) {
    (
        e.charts.charts.iter().map(|c| c.columns.len()).sum(),
        e.charts.hits.len(),
    )
}

fn live(e: &Engine) {
    let (columns, hits) = shape(e);
    assert!(columns > 0 && hits > 0, "columns={columns} hits={hits}");
}

#[test]
fn partial_selection_keeps_the_remainder_and_only_final_learning_discards() {
    let mut e = engine();
    type_into(&mut e, "nihaoshijie");
    pick(&mut e, "拟好", false);
    assert_eq!(e.composition().unwrap().raw, "shijie");
    assert!(e.composition_committed());
    assert_eq!(e.context(), "拟好");
    live(&e);
    assert_eq!(e.charts.user.0, e.user.read().unwrap().revision());
    let caps = (
        e.charts.charts.capacity(),
        e.charts.hits.capacity(),
        e.cands.capacity(),
    );
    pick(&mut e, "世界", true);
    assert!(e.composition().is_none());
    assert_eq!(e.context(), "拟好世界");
    assert!(e.charts.user.0 < e.user.read().unwrap().revision());
    assert_eq!(shape(&e), (0, 0));
    assert_eq!(
        caps,
        (
            e.charts.charts.capacity(),
            e.charts.hits.capacity(),
            e.cands.capacity()
        )
    );
    let words = e.user.read().unwrap().export();
    for record in [
        "w\tni'hao\t拟好\t1\t",
        "w\tshi'jie\t世界\t1\t",
        "w\tni'hao'shi'jie\t拟好世界\t1\t",
        "r\tnihaoshijie\t拟好世界\t1\t",
        "t\t拟好\t世界\t1\t",
    ] {
        assert!(words.contains(record), "{words}");
    }
}

#[test]
fn fragment_word_bonus_requires_matching_context_after_profile_import() {
    let mut trained = engine();
    for _ in 0..3 {
        trained.set_context("今天");
        type_into(&mut trained, "nihaoshijie");
        pick(&mut trained, "拟好", false);
        pick(&mut trained, "世界", true);
    }
    let profile = trained.export_user();
    assert!(profile.contains("w\tni'hao\t拟好\t3\t"), "{profile}");
    assert!(
        profile.lines().any(|line| {
            line.starts_with("f\tni'hao\t拟好\t3\t") && line.ends_with("\t今天")
        }),
        "{profile}"
    );

    let mut restored = engine();
    restored.import_user(&profile);
    assert_eq!(restored.export_user(), profile);
    for (context, boosted) in [("是今天", true), ("明天", false), ("今天。", false)] {
        restored.set_context(context);
        type_into(&mut restored, "nh");
        let cand = restored
            .cands
            .iter()
            .find(|c| c.full && c.text == "拟好")
            .unwrap();
        assert_eq!(cand.personal > 0.0, boosted, "{context}: {cand:?}");
        assert!(restored.process(Key::Esc).is_empty());
    }
}

#[test]
fn cancel_raw_and_context_only_commit_keep_reusable_entries() {
    let mut e = engine();
    e.set_learning(false);
    e.set_context("今天");
    type_into(&mut e, "nihao");
    live(&e);
    let before = shape(&e);
    assert!(e.process(Key::Esc).is_empty());
    assert_eq!(shape(&e), before);
    type_into(&mut e, "nihao");
    assert_eq!(e.finalize_raw().as_deref(), Some("nihao"));
    assert_eq!(shape(&e), before);
    type_into(&mut e, "nihao");
    pick(&mut e, "你好", true);
    assert_eq!(e.context(), "今天你好");
    assert_eq!(shape(&e), before);
    // A real host may set exactly the previous context at next composition start.
    e.set_context("今天");
    type_into(&mut e, "nihao");
    assert_eq!(e.cands[0].text, "你好");
    assert!(e.user.read().unwrap().export().is_empty());
}

#[test]
fn learned_lock_in_a_clause_requires_literal_key_evidence() {
    let mut e = engine();
    {
        let mut user = e.user.write().unwrap();
        for _ in 0..3 {
            user.learn("ni'hao", "拟好");
            user.learn_raw("nihao", "拟好");
        }
    }
    for (context, typed, locked) in [
        ("昨天。", "nh", true),
        ("今天", "nh", false),
        ("今天", "nihao", true),
    ] {
        e.set_context(context);
        type_into(&mut e, typed);
        let cand = e.cands.iter().find(|c| c.full && c.text == "拟好").unwrap();
        assert!(
            cand.trusted && cand.learned && cand.personal > 0.0,
            "{cand:?}"
        );
        assert!(cand.level >= crate::user::PROMOTE_LEVEL, "{cand:?}");
        assert_eq!(cand.learned_enough(), locked, "{context:?}, {typed}");
        assert!(e.process(Key::Esc).is_empty());
    }
}

#[test]
fn foreign_learning_changes_ranking_after_esc_but_does_not_touch_active_charts() {
    let mut a = engine();
    let mut b = engine().with_user(a.user.clone());
    type_into(&mut a, "nihao");
    assert_eq!(a.cands[0].text, "你好");
    let before = shape(&a);
    for _ in 0..3 {
        type_into(&mut b, "nihao");
        pick(&mut b, "拟好", true);
    }
    assert!(a.process(Key::Left).is_empty()); // cursor motion, not a new decode
    assert_eq!(shape(&a), before);
    assert!(a.process(Key::Esc).is_empty());
    assert_eq!(shape(&a), (0, 0));
    type_into(&mut a, "nihao");
    assert_eq!(a.cands[0].text, "拟好");
    assert!(a.cands[0].learned_enough());
}

#[test]
fn stale_raw_and_empty_buffer_exits_check_the_same_real_transition() {
    for path in 0..4 {
        let mut e = engine();
        type_into(&mut e, "n");
        live(&e);
        e.user.write().unwrap().learn("ni", "拟"); // another host
        match path {
            0 => assert_eq!(e.process(Key::Backspace), []),
            1 => assert_eq!(e.process(Key::Enter), [Event::Commit("n".into())]),
            2 => assert_eq!(e.finalize_raw().as_deref(), Some("n")),
            _ => e.set_mode(crate::Mode::English),
        }
        assert!(e.composition().is_none());
        assert_eq!(shape(&e), (0, 0));
    }
    // Calling clear_comp with *no* composition is not a finish event.
    let mut e = engine();
    type_into(&mut e, "nihao");
    e.process(Key::Esc);
    let before = shape(&e);
    e.user.write().unwrap().learn("ni", "拟");
    e.set_mode(crate::Mode::English);
    assert_eq!(shape(&e), before);
}

#[test]
fn segmented_candidates_are_reachable_before_whole_reading_overflow() {
    let mut e = engine();
    e.set_page_size(5);
    type_into(&mut e, "nihao");
    let single = e
        .cands
        .iter()
        .position(|c| !c.full && c.text == "你")
        .unwrap();
    assert_eq!(single, 8);
    let tail = e
        .cands
        .iter()
        .enumerate()
        .skip(single + 1)
        .find(|(_, c)| c.full && !c.weaker_reading)
        .map(|(i, c)| (i, c.text.clone()))
        .unwrap();
    assert!(e.promote(tail.0));
    assert_eq!(e.cands[0].text, tail.1);
    assert_eq!(e.cands[single].text, "你");
    assert!(e.set_highlight(single));
    assert_eq!(e.pick_in_page(single % 5), [Event::Commit("你".into())]);
    assert_eq!(e.composition().unwrap().raw, "hao");

    let mut e = engine();
    type_into(&mut e, "nihaoma");
    let pair = e
        .cands
        .iter()
        .position(|c| !c.full && c.text == "你好")
        .unwrap();
    let single = e
        .cands
        .iter()
        .position(|c| !c.full && c.text == "你")
        .unwrap();
    assert_eq!(pair, 8);
    assert!(pair < single && single <= 12);
    assert!(e.cands[single + 1..].iter().any(|c| c.full));
    assert_eq!(e.pick(pair), [Event::Commit("你好".into())]);
    assert_eq!(e.composition().unwrap().raw, "ma");
}
