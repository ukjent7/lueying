use std::sync::Arc;

use crate::{Config, Engine, Event, Key};
use lueying_dict::Dictionary;

fn engine() -> Engine {
    let (d, bad) = Dictionary::from_text(
        "你\t9000\tni\n拟\t600\tni\n好\t9000\thao\n你好\t5000\tni'hao\n拟好\t800\tni'hao\n\
         世\t3000\tshi\n界\t2500\tjie\n世界\t4000\tshi'jie\n视界\t400\tshi'jie\n",
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
