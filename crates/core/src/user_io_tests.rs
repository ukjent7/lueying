//! 导入导出的测试:往返逐位相等,坏行跳过,已有记录按较新的那份合并。

use lueying_dict::syllables::FUZZY_GROUPS;

use crate::UserDict;
use crate::user::set_sim_now;

const DAY: u64 = 86_400;
const T0: u64 = 1_800_000_000;

#[test]
fn round_trip_is_lossless() {
    set_sim_now(T0);
    let mut user = UserDict::new();
    user.learn("ni'hao", "你好");
    user.learn("ni'hao", "你好");
    user.mispick("ni'hao", "你好");
    user.learn("ni'hao", "你好");
    user.learn("ni'hao", "泥好");
    set_sim_now(T0 + 9 * DAY);
    user.learn("ni'hao", "你好");
    user.learn_transitions(Some("你好"), &["世界".to_string()]);
    user.learn_raw("nihao", "你好", "");
    user.learn_raw("nihao", "泥好", "");
    user.learn_groups(1, "");
    user.learn_literal("rq", "!date");
    user.pin("ni", "你");
    let text = user.export();
    let mut again = UserDict::new();
    again.import(&text);
    assert_eq!(again.export(), text);
    set_sim_now(0);
}

#[test]
fn bad_lines_are_skipped() {
    let mut user = UserDict::new();
    user.import(
        "w\tni\t你\tx\t1\t1\t0\nt\t你\t\t1\t1\t1\nold\t你\t3\t10\nw\tni\t你\t2\t1\t100\t0\n",
    );
    let text = user.export();
    assert_eq!(text, "w\tni\t你\t2\t1\t100\t0\n");
}

#[test]
fn existing_records_take_the_more_recent_one() {
    let mut user = UserDict::new();
    user.import("w\tni\t你\t5\t2.5\t200\t0\n");
    user.import("w\tni\t你\t3\t9\t100\t0\n");
    assert_eq!(user.export(), "w\tni\t你\t5\t2.5\t200\t0\n");
}

#[test]
fn group_lines_keep_known_groups_only() {
    let mut user = UserDict::new();
    user.import(&format!("g\t{}\t2\t10\ng\tnope\t2\t10\n", FUZZY_GROUPS[0]));
    assert_eq!(user.export(), format!("g\t{}\t2\t10\n", FUZZY_GROUPS[0]));
}

#[test]
fn correction_evidence_round_trips_and_reset_clears_it() {
    set_sim_now(T0);
    let mut user = UserDict::new();
    user.learn("ni'hao", "你好");
    user.correction("ni'hao", "你好");
    assert!(user.promoted("ni'hao", "你好"), "一次纠正应达到首位门槛");
    let text = user.export();
    assert!(text.contains("x\tni'hao\t你好\t1\t"), "{text}");
    let mut again = UserDict::new();
    again.import(&text);
    assert_eq!(again.export(), text);
    assert!(again.promoted("ni'hao", "你好"));
    again.reset();
    assert!(again.is_empty());
    assert_eq!(again.export(), "");
    set_sim_now(0);
}

#[test]
fn mispick_withdraws_the_correction() {
    set_sim_now(T0);
    let mut user = UserDict::new();
    user.learn("ni'hao", "你好");
    user.correction("ni'hao", "你好");
    user.mispick("ni'hao", "你好");
    assert!(!user.promoted("ni'hao", "你好"), "误选后纠正证据应撤回");
    set_sim_now(0);
}

#[test]
fn correction_half_life_decays() {
    set_sim_now(T0);
    let mut user = UserDict::new();
    user.learn("ni'hao", "你好");
    user.correction("ni'hao", "你好");
    set_sim_now(T0 + 42 * DAY);
    assert!(
        !user.promoted("ni'hao", "你好"),
        "六周后纠正权重减半,不再单独升首位"
    );
    set_sim_now(0);
}
