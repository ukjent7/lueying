//! 子命令参数解析的回归:各子命令的位置参数、旗标与错误路径。

use std::path::PathBuf;

use super::{
    InstallArgs, Invocation, LogsArgs, NewArgs, PackArgs, ReplayArgs, TopOptions, parse_args,
};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|item| item.to_string()).collect()
}

#[test]
fn parses_top_and_help() {
    assert_eq!(
        parse_args(args(&["ext", "top", "--json", "--watch"])),
        Ok(Invocation::Top(TopOptions {
            file: None,
            json: true,
            watch: true,
        }))
    );
    assert_eq!(parse_args(args(&[])), Ok(Invocation::Help));
    assert_eq!(parse_args(args(&["-h"])), Ok(Invocation::Help));
    assert_eq!(parse_args(args(&["ext", "--help"])), Ok(Invocation::Help));
    assert!(parse_args(args(&["ext"])).is_err(), "缺子命令要报错");
    assert!(parse_args(args(&["ext", "nope"])).is_err());
    assert!(parse_args(args(&["run"])).is_err());
}

#[test]
fn parses_new() {
    assert_eq!(
        parse_args(args(&["ext", "new", "demo", "--name", "示例", "--dir", "/tmp/x"])),
        Ok(Invocation::New(NewArgs {
            id: "demo".into(),
            name: Some("示例".into()),
            dir: Some(PathBuf::from("/tmp/x")),
        }))
    );
    let bare = parse_args(args(&["ext", "new", "demo"]));
    assert_eq!(
        bare,
        Ok(Invocation::New(NewArgs {
            id: "demo".into(),
            name: None,
            dir: None,
        }))
    );
    assert!(parse_args(args(&["ext", "new"])).is_err(), "缺 id");
    assert!(
        parse_args(args(&["ext", "new", "a", "b"])).is_err(),
        "两个位置参数要报错"
    );
    assert!(parse_args(args(&["ext", "new", "--name"])).is_err(), "--name 缺值");
}

#[test]
fn parses_pack_and_install() {
    assert_eq!(
        parse_args(args(&["ext", "pack", "demo", "--out", "d.luext"])),
        Ok(Invocation::Pack(PackArgs {
            dir: PathBuf::from("demo"),
            out: Some(PathBuf::from("d.luext")),
        }))
    );
    assert!(parse_args(args(&["ext", "pack"])).is_err(), "缺目录");
    assert_eq!(
        parse_args(args(&["ext", "install", "demo.luext", "--force"])),
        Ok(Invocation::Install(InstallArgs {
            source: PathBuf::from("demo.luext"),
            dir: None,
            force: true,
        }))
    );
    assert_eq!(
        parse_args(args(&["ext", "install", "demo", "--dir", "/tmp/e"])),
        Ok(Invocation::Install(InstallArgs {
            source: PathBuf::from("demo"),
            dir: Some(PathBuf::from("/tmp/e")),
            force: false,
        }))
    );
    assert!(parse_args(args(&["ext", "install"])).is_err(), "缺源");
}

#[test]
fn parses_reload_and_logs() {
    assert_eq!(parse_args(args(&["ext", "reload"])), Ok(Invocation::Reload));
    assert!(
        parse_args(args(&["ext", "reload", "extra"])).is_err(),
        "reload 不接受参数"
    );
    assert_eq!(
        parse_args(args(&["ext", "logs", "--tail", "5"])),
        Ok(Invocation::Logs(LogsArgs {
            file: None,
            tail: 5,
        }))
    );
    assert!(parse_args(args(&["ext", "logs", "--tail", "行数"])).is_err());
    assert!(parse_args(args(&["ext", "logs", "--tail"])).is_err(), "--tail 缺值");
    assert!(parse_args(args(&["ext", "logs", "--wat"])).is_err(), "未知旗标");
}

#[test]
fn parses_replay() {
    assert_eq!(
        parse_args(args(&[
            "ext",
            "replay",
            "script.txt",
            "--trigger",
            "ocr=demo.run",
            "--config",
            "c.toml"
        ])),
        Ok(Invocation::Replay(ReplayArgs {
            script: Some(PathBuf::from("script.txt")),
            config: Some(PathBuf::from("c.toml")),
            triggers: vec![("ocr".into(), "demo.run".into())],
        }))
    );
    assert_eq!(
        parse_args(args(&["ext", "replay"])),
        Ok(Invocation::Replay(ReplayArgs {
            script: None,
            config: None,
            triggers: Vec::new(),
        }))
    );
    assert!(
        parse_args(args(&["ext", "replay", "--trigger", "ocr"])).is_err(),
        "触发词缺 = 要报错"
    );
    assert!(
        parse_args(args(&["ext", "replay", "--trigger", "=run"])).is_err(),
        "空触发词要报错"
    );
}
