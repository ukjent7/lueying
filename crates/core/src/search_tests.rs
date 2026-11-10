//! 增量解码的等价性:组合每变一步,候选都必须与同一串输入从头键入得到的候选逐项相同。
//! 另锁定零声母的 e/o 简拼:`eqh` 读作「而且还」。

use std::sync::Arc;

use lueying_dict::Dictionary;

use crate::{Config, Engine, Key};

const WORDS: &str = "\
你\t9000\tni
好\t9000\thao
你好\t5000\tni'hao
拟好\t800\tni'hao
是\t9000\tshi
世界\t3000\tshi'jie
时界\t500\tshi'jie
而\t3000\ter
且\t2000\tqie
还\t2000\thai
而且还\t300\ter'qie'hai
中国\t4000\tzhong'guo
中过\t300\tzhong'guo
总过\t200\tzong'guo
人\t8000\tren
民\t6000\tmin
人民\t4000\tren'min
";

fn dictionary() -> Arc<Dictionary> {
    let (dict, bad) = Dictionary::from_text(WORDS);
    assert_eq!(bad, 0);
    Arc::new(dict)
}

fn engine(dict: &Arc<Dictionary>) -> Engine {
    let mut engine = Engine::new(Arc::clone(dict), Config::default());
    engine.set_learning(false);
    engine
}

/// 候选的可比较摘要:文字、分数的位模式、切分区间与标记。
fn digest(engine: &Engine) -> Vec<String> {
    engine
        .candidates_detailed()
        .iter()
        .map(|cand| {
            format!(
                "{}|{:x}|{:?}|{}|{}|{}",
                cand.text,
                cand.score.to_bits(),
                cand.spans,
                cand.full,
                cand.correction,
                cand.completion
            )
        })
        .collect()
}

/// 同一串字母从头键入的候选。
fn fresh(dict: &Arc<Dictionary>, raw: &str) -> Engine {
    let mut reference = engine(dict);
    for c in raw.chars() {
        reference.process(Key::Letter(c));
    }
    reference
}

/// 确定性的伪随机数,测试可复现。
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % bound
    }
}

#[test]
fn incremental_candidates_equal_fresh_ones_under_edits() {
    const KEYS: &str = "nihaoshijierenminzhongguoqtyp";
    let dict = dictionary();
    let mut typing = engine(&dict);
    let mut rng = Lcg(0x5eed);
    let mut checked = 0;
    for _ in 0..1500 {
        let key = match rng.next(10) {
            0 => Key::Backspace,
            1 => Key::Left,
            2 => Key::Right,
            3 => Key::Delete,
            4 => Key::Home,
            5 => Key::End,
            _ => Key::Letter(KEYS.as_bytes()[rng.next(KEYS.len())] as char),
        };
        typing.process(key);
        let Some(comp) = typing.composition() else {
            continue;
        };
        let raw = comp.raw.clone();
        assert_eq!(digest(&typing), digest(&fresh(&dict, &raw)), "组合 {raw}");
        checked += 1;
    }
    assert!(checked > 500);
}

#[test]
fn incremental_candidates_follow_context_changes() {
    let dict = dictionary();
    let mut typing = engine(&dict);
    typing.set_context("今天");
    for c in "renmin".chars() {
        typing.process(Key::Letter(c));
    }
    let mut reference = engine(&dict);
    reference.set_context("今天");
    for c in "renmin".chars() {
        reference.process(Key::Letter(c));
    }
    assert_eq!(digest(&typing), digest(&reference));
    // 清掉上文后按同一串输入重算,结果必须与无上文时从头键入一致。
    typing.clear_context();
    typing.process(Key::Backspace);
    typing.process(Key::Letter('n'));
    assert_eq!(digest(&typing), digest(&fresh(&dict, "renmin")));
}

#[test]
fn vowel_initials_abbreviate_zero_initial_words() {
    let dict = dictionary();
    let mut typing = engine(&dict);
    for c in "eqh".chars() {
        typing.process(Key::Letter(c));
    }
    let candidates = typing.candidates();
    assert!(
        candidates.iter().any(|text| text == "而且还"),
        "{candidates:?}"
    );
    assert_eq!(digest(&typing), digest(&fresh(&dict, "eqh")));
}
