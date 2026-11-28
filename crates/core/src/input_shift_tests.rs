//! 组合中 Shift / 大写字母 / 外部切模式(CapsLock)的原样上屏路径。

use std::sync::Arc;

use crate::{Config, Engine, Event, Key, Mode};
use lueying_dict::Dictionary;

fn engine() -> Engine {
    let (dict, bad) = Dictionary::from_text(
        "你\t9000\tni\n拟\t600\tni\n好\t9000\thao\n你好\t5000\tni'hao\n拟好\t800\tni'hao\n",
    );
    assert_eq!(bad, 0);
    Engine::new(Arc::new(dict), Config::default())
}

fn type_into(e: &mut Engine, raw: &str) {
    for ch in raw.chars() {
        assert!(e.process(Key::Letter(ch)).is_empty());
    }
}

#[test]
fn shift_during_composition_commits_raw_then_switches_english() {
    let mut e = engine();
    type_into(&mut e, "nihao");
    assert_eq!(e.candidates().first().map(String::as_str), Some("你好"));
    assert_eq!(
        e.process(Key::Shift),
        [Event::Commit("nihao".into()), Event::Mode(Mode::English),]
    );
    assert!(e.composition().is_none());
    assert_eq!(e.mode(), Mode::English);
    // 切到英文后的下一键直接交给宿主,不再经过中文候选。
    assert_eq!(
        e.process(Key::Letter('A')),
        [Event::Passthrough(Key::Letter('A'))]
    );
    // 原样上屏不是选词,不写用户词库。
    assert!(e.user.read().unwrap().export().is_empty());
}

#[test]
fn uppercase_letter_during_composition_commits_raw_then_uppercase() {
    let mut e = engine();
    type_into(&mut e, "ni");
    assert_eq!(e.candidates().first().map(String::as_str), Some("你"));
    assert_eq!(
        e.process(Key::Letter('A')),
        [Event::Commit("ni".into()), Event::Commit("A".into())]
    );
    assert!(e.composition().is_none());
    assert_eq!(e.mode(), Mode::Chinese);
    assert!(e.user.read().unwrap().export().is_empty());
}

#[test]
fn caps_lock_switch_path_commits_raw_before_english() {
    // CapsLock 由 TSF 发 Request::SetMode;Server 的 switch_mode 对中文组合
    // 调用 process(Key::Shift)。引擎层能验证的就是这条统一切换路径:
    // 先原样上屏,再切英文;之后的字母由大写锁定直接给出大写。
    let mut e = engine();
    type_into(&mut e, "nihao");
    assert_eq!(
        e.process(Key::Shift),
        [Event::Commit("nihao".into()), Event::Mode(Mode::English),]
    );
    assert!(e.composition().is_none());
    assert_eq!(e.mode(), Mode::English);
    assert_eq!(
        e.process(Key::Letter('A')),
        [Event::Passthrough(Key::Letter('A'))]
    );
}
