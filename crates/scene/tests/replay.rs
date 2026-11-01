//! 回放脚本:解析报错、事件词表与确定性推演。

use lueying_scene::replay::{event_of, name_of, parse, replay};
use lueying_scene::{EventName, Layer, parse as parse_scene};

const SCENE: &str = r#"# 场景 = 候选窗的装饰层。改完存盘一秒内热重载;字段全表见 docs。
[signals.energy]
from = "typing_rate"
gain = 0.6
decay = "1.5s"

[[nodes]]
id = "glow"
kind = "shape"
shape = "rrect"
anchor = "highlight"
inset = -5
radius = 6
fill = "accent"
opacity = "lerp(0.15, 0.55, energy)"
effect = { kind = "blur", radius = "lerp(2, 10, energy)" }
"#;

fn scene() -> lueying_scene::Scene {
    parse_scene("scene.toml", SCENE).expect("场景要合法")
}

#[test]
fn script_errors_are_chinese_and_stepped() {
    let errors = parse("a.toml", "[[steps]]\ndt = -1.0\n").expect_err("要报错");
    assert!(errors[0].message.contains("第 1 步"), "{}", errors[0].message);
    assert!(errors[0].message.contains("dt"), "{}", errors[0].message);
    let errors = parse("a.toml", "[[steps]]\ndt = 2.0\n").expect_err("要报错");
    assert!(errors[0].message.contains("dt = 2"), "{}", errors[0].message);
    let errors = parse("a.toml", "[[steps]]\ndt = 0.1\ncandidates = 99\n").expect_err("要报错");
    assert!(
        errors[0].message.contains("candidates"),
        "{}",
        errors[0].message
    );
    let errors = parse("a.toml", "[[steps]]\ndt = 0.1\nwho = 1\n").expect_err("要报错");
    assert!(
        errors[0].message.contains("TOML 解析失败"),
        "{}",
        errors[0].message
    );
    let errors = parse("a.toml", "[[steps]]\ndt = 0.1\npage_count = 0\n").expect_err("要报错");
    assert!(
        errors[0].message.contains("page_count"),
        "{}",
        errors[0].message
    );
}

#[test]
fn event_names_round_trip() {
    assert_eq!(event_of("backspace_burst"), EventName::BackspaceBurst);
    assert_eq!(event_of("party"), EventName::Custom("party".into()));
    assert_eq!(name_of(&EventName::ModeSwitch), "mode_switch");
    assert_eq!(name_of(&event_of("party")), "party");
}

#[test]
fn replay_is_deterministic() {
    let script = parse(
        "a.toml",
        "[[steps]]\ndt = 0.1\ncandidates = 2\ntyping_rate = 4.0\nevents = [\"key\"]\n\n[[steps]]\ndt = 0.2\nevents = [\"commit\"]\n",
    )
    .expect("脚本要合法");
    let first = replay(&script, scene());
    let second = replay(&script, scene());
    assert_eq!(first, second);
    assert_eq!(first.len(), 2);
}

#[test]
fn frames_follow_steps_in_order() {
    let script = parse(
        "a.toml",
        "[[steps]]\ndt = 0.016\ncandidates = 3\nselected = 1\nevents = [\"key\"]\n",
    )
    .expect("脚本要合法");
    let frames = replay(&script, scene());
    assert_eq!(frames.len(), 1);
    assert!(!frames[0].degraded);
    // below-text 图元排在前;场景层的层序契约与候选窗一致。
    if let [first, ..] = &frames[0].items[..] {
        assert_eq!(first.layer, Layer::BelowText);
    }
}

#[test]
fn empty_script_is_zero_frames() {
    let script = parse("a.toml", "").expect("空脚本合法");
    assert!(replay(&script, scene()).is_empty());
}
