//! 官方示例既是文档也是回归测试:每个场景都要合法,关键字段要出帧。

use std::fs;
use std::path::Path;

use lueying_scene::{parse, Body, EventName, Inputs, Layer, Runtime};

const HIGHLIGHT: [f32; 4] = [8.0, 36.0, 100.0, 30.0];

fn inputs<'a>(candidates: &'a [[f32; 4]]) -> Inputs<'a> {
    Inputs {
        typing_rate: 2.0,
        candidates: candidates.len(),
        page: 0,
        page_count: 2,
        mode_chinese: false,
        selected: 0,
        idle: 0.0,
        panel: [0.0, 0.0, 400.0, 100.0],
        preedit: [0.0, 0.0, 400.0, 32.0],
        highlight: candidates.first().copied(),
        candidates,
        caret: [0.0, 0.0, 0.0, 20.0],
        screen: [0.0, 0.0, 400.0, 100.0],
    }
}

fn read(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scenes")
            .join(name),
    )
    .expect("示例在")
}

fn build(name: &str) -> Runtime {
    let text = read(name);
    match parse(name, &text) {
        Ok(scene) => Runtime::new(scene),
        Err(errors) => panic!(
            "官方示例 {name} 不合法:{}",
            errors
                .iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join(";")
        ),
    }
}

#[test]
fn official_scenes_are_valid_and_all_present() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenes");
    let mut checked = 0;
    for entry in fs::read_dir(dir).expect("scenes 目录在") {
        let path = entry.expect("目录项").path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let name = path.file_name().expect("文件名").to_string_lossy().into_owned();
        build(&name);
        checked += 1;
    }
    assert!(checked >= 5, "官方示例至少 5 个,只有 {checked} 个");
}

#[test]
fn glow_expands_around_the_highlight() {
    let mut runtime = build("glow.toml");
    let frame = runtime.tick(0.0, 0.016, &inputs(&[HIGHLIGHT]), &[]);
    assert_eq!(frame.items.len(), 1);
    assert_eq!(frame.items[0].layer, Layer::BelowText);
    // inset = -5:高亮块四周外扩 5 逻辑像素。
    assert!((frame.items[0].rect[0] - (HIGHLIGHT[0] - 5.0)).abs() < 1e-4);
}

#[test]
fn carousel_switches_by_page() {
    let mut runtime = build("carousel.toml");
    let candidates = [HIGHLIGHT];
    let mut page_inputs = inputs(&candidates);
    page_inputs.page = 1;
    let frame = runtime.tick(0.0, 0.016, &page_inputs, &[]);
    let Body::Image { src } = &frame.items[0].body else {
        panic!("要出图片");
    };
    assert_eq!(src, "bg-1.png");
}

#[test]
fn mascot_cheers_on_commit() {
    let mut runtime = build("mascot.toml");
    let candidates = [HIGHLIGHT];
    runtime.tick(0.0, 0.016, &inputs(&candidates), &[EventName::Commit]);
    let frame = runtime.tick(0.016, 0.1, &inputs(&candidates), &[]);
    let Body::Sprite { frame: index, .. } = &frame.items[0].body else {
        panic!("要出精灵");
    };
    assert_eq!(*index, 1, "庆祝片段第 1 帧");
}
