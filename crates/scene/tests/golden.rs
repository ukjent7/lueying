//! 黄金测试:同输入必同输出;变量、规则、补间、粒子按声明的语义走。
//!
//! 运行时是纯确定性的,这里不需要真机:喂事件序列,断言图元清单。

use lueying_scene::{parse, Body, EventName, Inputs, Layer, Runtime};

const PANEL: [f32; 4] = [0.0, 0.0, 400.0, 100.0];
const PREEDIT: [f32; 4] = [0.0, 0.0, 400.0, 32.0];
const HIGHLIGHT: [f32; 4] = [8.0, 36.0, 100.0, 30.0];

fn inputs<'a>(candidates: &'a [[f32; 4]]) -> Inputs<'a> {
    Inputs {
        typing_rate: 0.0,
        candidates: candidates.len(),
        page: 0,
        page_count: 1,
        mode_chinese: true,
        selected: 0,
        idle: 0.0,
        panel: PANEL,
        preedit: PREEDIT,
        highlight: candidates.first().copied(),
        candidates,
        caret: [0.0, 0.0, 0.0, 20.0],
        screen: PANEL,
    }
}

fn build(text: &str) -> Runtime {
    Runtime::new(parse("scene.toml", text).expect("场景要合法"))
}

#[test]
fn load_errors_are_chinese_and_located() {
    let errors = parse("scene.toml", "
[[nodes]]
id = \"glow\"
kind = \"shape\"
fill = \"accentt\"
[[nodes]]
id = \"glow\"
kind = \"shape\"
opacity = \"power\"
")
    .expect_err("要报错");
    assert!(errors.iter().any(|error| error.to_string().contains("scene.toml")));
    assert!(errors.iter().any(|error| error.message.contains("不认识的颜色")));
    assert!(errors.iter().any(|error| error.message.contains("power")));
    // 重复 id 与未知信号都要点出节点。
    assert!(errors.iter().any(|error| error.message.contains("glow")));
}

#[test]
fn unknown_signal_reports_the_name() {
    let errors = parse("s.toml", "[[nodes]]\nid = \"x\"\nkind = \"shape\"\nopacity = \"nope + 1\"\n").unwrap_err();
    assert!(errors[0].message.contains("nope"));
}

#[test]
fn shape_anchors_to_highlight_and_expands_with_inset() {
    let mut runtime = build("
[[nodes]]
id = \"glow\"
kind = \"shape\"
shape = \"rrect\"
anchor = \"highlight\"
inset = -4
");
    let frame = runtime.tick(0.0, 0.016, &inputs(&[HIGHLIGHT]), &[]);
    assert_eq!(frame.items.len(), 1);
    let item = &frame.items[0];
    assert_eq!(item.layer, Layer::BelowText);
    // inset = -4:四周外扩 4 逻辑像素。
    assert!((item.rect[0] - (HIGHLIGHT[0] - 4.0)).abs() < 1e-4);
    assert!((item.rect[2] - (HIGHLIGHT[2] + 8.0)).abs() < 1e-4);
}

#[test]
fn variable_rises_and_decays() {
    let mut runtime = build("
[signals.energy]
from = \"typing_rate\"
gain = 1.0
decay = \"1s\"
[[nodes]]
id = \"glow\"
kind = \"shape\"
opacity = \"energy\"
");
    let mut inputs = inputs(&[HIGHLIGHT]);
    inputs.typing_rate = 1.0;
    let first = runtime.tick(0.0, 0.5, &inputs, &[]);
    // gain = 1,dt = 0.5:halfway 到目标。
    let first = &first.items[0];
    assert!((first.opacity - 0.5).abs() < 1e-4, "opacity = {}", first.opacity);
    // 停止打字:energy 指数衰减,半衰期由 decay 决定。
    inputs.typing_rate = 0.0;
    let second = runtime.tick(0.5, 0.5, &inputs, &[]);
    let second = &second.items[0];
    assert!(second.opacity < first.opacity);
}

#[test]
fn rules_set_variables_and_chain_events() {
    let mut runtime = build("
[signals.flash]
from = \"0\"
[[nodes]]
id = \"glow\"
kind = \"shape\"
opacity = \"flash\"
[[rules]]
on = \"commit\"
do = [ { set = \"flash\", to = 1.0 }, { emit = \"flash.started\" } ]
[[rules]]
on = \"flash.started\"
do = [ { set = \"flash\", to = 0.5 } ]
");
    // set 的值这一帧保持;emit 出来的事件同帧接着处理,后一条规则
    // 看到 1.0 又写成 0.5。下一帧积分把 from=0 拉回来。
    let frame = runtime.tick(0.0, 0.016, &inputs(&[HIGHLIGHT]), &[EventName::Commit]);
    assert!((frame.items[0].opacity - 0.5).abs() < 1e-4);
    let frame = runtime.tick(0.016, 0.016, &inputs(&[HIGHLIGHT]), &[]);
    assert!(frame.items[0].opacity < 1.0);
}

#[test]
fn tween_moves_property_over_time() {
    let mut runtime = build("
[[nodes]]
id = \"glow\"
kind = \"shape\"
opacity = \"0.2\"
[[rules]]
on = \"commit\"
do = [ { tween = \"glow.opacity\", to = 1.0, over = \"1s\", ease = \"linear\" } ]
");
    runtime.tick(0.0, 0.0, &inputs(&[HIGHLIGHT]), &[EventName::Commit]);
    let frame = runtime.tick(0.5, 0.5, &inputs(&[HIGHLIGHT]), &[]);
    assert!((frame.items[0].opacity - 0.6).abs() < 1e-4, "{}", frame.items[0].opacity);
    let frame = runtime.tick(1.0, 0.5, &inputs(&[HIGHLIGHT]), &[]);
    assert!((frame.items[0].opacity - 1.0).abs() < 1e-4);
}

#[test]
fn emitter_spawns_and_fades_deterministically() {
    let text = "
[[nodes]]
id = \"stars\"
kind = \"emitter\"
rate = \"120\"
life = \"0.5s\"
";
    let mut a = build(text);
    let mut b = build(text);
    let events = [EventName::Commit];
    for step in 0..10u32 {
        let time = f64::from(step) * 0.016;
        let frame_a = a.tick(time, 0.016, &inputs(&[HIGHLIGHT]), &events);
        let frame_b = b.tick(time, 0.016, &inputs(&[HIGHLIGHT]), &events);
        assert_eq!(frame_a, frame_b, "第 {step} 帧要逐位一致");
    }
    // 120/秒、寿命 0.5s:稳态约 60 颗;粒子随年龄淡出。
    let frame = a.tick(10.0, 0.016, &inputs(&[HIGHLIGHT]), &[]);
    let Body::Particles { particles, .. } = &frame.items[0].body else {
        panic!("要出粒子");
    };
    assert!(!particles.is_empty());
    assert!(particles.len() < 70);
}

#[test]
fn sprite_plays_clip_and_stops_on_last_frame() {
    let mut runtime = build("
[[nodes]]
id = \"mochi\"
kind = \"sprite\"
src = \"mochi.png\"
grid = [32, 32]
fps = 10
[nodes.clips.cheer]
frames = [1, 2, 3]
fps = 10
loop = false
[[rules]]
on = \"commit\"
do = [ { play = \"mochi.cheer\" } ]
");
    runtime.tick(0.0, 0.0, &inputs(&[HIGHLIGHT]), &[EventName::Commit]);
    let frame = runtime.tick(0.0, 0.05, &inputs(&[HIGHLIGHT]), &[]);
    let Body::Sprite { frame: index, .. } = &frame.items[0].body else {
        panic!("要出精灵");
    };
    assert_eq!(*index, 1);
    let frame = runtime.tick(0.05, 0.5, &inputs(&[HIGHLIGHT]), &[]);
    let Body::Sprite { frame: index, .. } = &frame.items[0].body else {
        panic!("要出精灵");
    };
    assert_eq!(*index, 3, "非循环片段停在最后一帧");
}

#[test]
fn above_text_sorts_after_below_text() {
    let mut runtime = build("
[[nodes]]
id = \"top\"
kind = \"shape\"
layer = \"above-text\"
[[nodes]]
id = \"bottom\"
kind = \"shape\"
");
    let frame = runtime.tick(0.0, 0.016, &inputs(&[HIGHLIGHT]), &[]);
    assert_eq!(frame.items[0].layer, Layer::BelowText);
    assert_eq!(frame.items[1].layer, Layer::AboveText);
}

#[test]
fn missing_anchor_skips_the_node() {
    let mut runtime = build("
[[nodes]]
id = \"glow\"
kind = \"shape\"
anchor = \"highlight\"
");
    let frame = runtime.tick(0.0, 0.016, &inputs(&[]), &[]);
    assert!(frame.items.is_empty(), "没有选中项就不画");
}

#[test]
fn invisible_node_is_skipped() {
    let mut runtime = build("
[[nodes]]
id = \"glow\"
kind = \"shape\"
visible = \"selected == 0\"
[[nodes]]
id = \"other\"
kind = \"shape\"
visible = \"selected > 0\"
");
    let frame = runtime.tick(0.0, 0.016, &inputs(&[HIGHLIGHT]), &[]);
    assert_eq!(frame.items.len(), 1);
}
