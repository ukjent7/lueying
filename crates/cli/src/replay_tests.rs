//! `replay` 的回归:按键行解析、无头回放输出、触发词注入与配置读取。

use std::fs;
use std::path::{Path, PathBuf};

use lueying_engine::Key;

use super::{ReplayArgs, parse_line, replay_with, run};

fn temp_root(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("lueying-replay-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("建临时目录");
    base
}

fn write_script(root: &Path, text: &str) -> PathBuf {
    let path = root.join("script.txt");
    fs::write(&path, text).expect("写脚本");
    path
}

fn ext_pack(root: &Path, id: &str, command: &str, title: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).expect("建扩展目录");
    let manifest = format!(
        r#"{{"id":"{id}","name":"演示","version":"1.0.0","api":2,"main":"index.js","activate":{{"commands":[{{"id":"{command}","title":"{title}"}}]}}}}"#
    );
    fs::write(dir.join("manifest.json"), manifest).expect("写清单");
}

fn args(script: &Path) -> ReplayArgs {
    ReplayArgs {
        script: Some(script.to_path_buf()),
        config: None,
        triggers: Vec::new(),
    }
}

#[test]
fn parse_line_maps_keys_and_rejects_junk() {
    assert_eq!(
        parse_line("nihao").expect("字母行"),
        vec![
            Key::Letter('n'),
            Key::Letter('i'),
            Key::Letter('h'),
            Key::Letter('a'),
            Key::Letter('o'),
        ]
    );
    assert_eq!(
        parse_line("<Space>,5'").expect("混合行"),
        vec![Key::Space, Key::Punct(','), Key::Digit(5), Key::Punct('\''),]
    );
    assert_eq!(
        parse_line("<esc><back>").expect("功能键行"),
        vec![Key::Esc, Key::Backspace]
    );
    let error = parse_line("nihao<space").expect_err("缺闭合要报错");
    assert!(error.contains("闭合"), "{error}");
    let error = parse_line("nihao!").expect_err("未知字符要报错");
    assert!(error.contains("看不懂"), "{error}");
    let error = parse_line("<ping>").expect_err("未知功能键要报错");
    assert!(error.contains("未知功能键"), "{error}");
}

#[test]
fn replay_types_and_commits() {
    let root = temp_root("types");
    let script = write_script(&root, "# 注释与空行被忽略\nnihao\n<space>\n");
    let out = run(&args(&script)).expect("回放应成功");
    assert!(out.contains("> nihao\n"), "{out}");
    assert!(out.contains("组合 nihao · 候选"), "{out}");
    assert!(out.contains("你好"), "{out}");
    assert!(out.contains("上屏 你好\n"), "{out}");
    assert!(out.ends_with("组合 (无)\n"), "{out}");
}

#[test]
fn replay_reports_line_numbers_on_bad_input() {
    let root = temp_root("badline");
    let script = write_script(&root, "nihao\n一下\n");
    let error = run(&args(&script)).expect_err("中文行要报错");
    assert!(error.contains("第 2 行"), "{error}");
}

#[test]
fn trigger_appears_and_fires_action() {
    let root = temp_root("trigger");
    let ext_root = root.join("extensions");
    ext_pack(&ext_root, "demo", "run", "运行一下");

    let script = write_script(&root, "nihao\n");
    let out = replay_with(
        &ReplayArgs {
            script: Some(script),
            config: None,
            triggers: vec![("nihao".into(), "demo.run".into())],
        },
        Some(&ext_root),
    )
    .expect("回放应成功");
    assert!(out.contains("运行一下"), "动作候选要出现:{out}");

    let script = write_script(&root, "nihao2\n");
    let out = replay_with(
        &ReplayArgs {
            script: Some(script),
            config: None,
            triggers: vec![("nihao".into(), "demo.run".into())],
        },
        Some(&ext_root),
    )
    .expect("回放应成功");
    assert!(out.contains("动作 demo.run"), "数字键要执行命令:{out}");
}

#[test]
fn config_triggers_load_and_flag_overrides() {
    let root = temp_root("config");
    let ext_root = root.join("extensions");
    ext_pack(&ext_root, "demo", "run", "运行一下");

    let config = root.join("config.toml");
    fs::write(&config, "[triggers]\nnihao = \"demo.missing\"\n").expect("写配置");
    let script = write_script(&root, "nihao\n");
    let out = replay_with(
        &ReplayArgs {
            script: Some(script.clone()),
            config: Some(config.clone()),
            triggers: Vec::new(),
        },
        Some(&ext_root),
    )
    .expect("回放应成功");
    assert!(
        !out.contains("运行一下"),
        "标题表里没有的命令不出动作候选:{out}"
    );

    let out = replay_with(
        &ReplayArgs {
            script: Some(script),
            config: Some(config),
            triggers: vec![("nihao".into(), "demo.run".into())],
        },
        Some(&ext_root),
    )
    .expect("回放应成功");
    assert!(out.contains("运行一下"), "命令行触发词要覆盖配置:{out}");
}
