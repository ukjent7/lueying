//! `extcmd` 的回归:骨架、打包、解包、安装与日志切片,全部走临时目录。

use std::fs;
use std::path::PathBuf;

use super::{
    InstallArgs, LogsArgs, NewArgs, PACK_HEADER, PackArgs, install, logs, new, pack, unpack,
};

fn temp_root(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("lueying-extcmd-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("建临时目录");
    base
}

fn skeleton(root: &PathBuf, id: &str) -> PathBuf {
    new(&NewArgs {
        id: id.to_string(),
        name: None,
        dir: Some(root.clone()),
    })
    .expect("new 应成功")
}

#[test]
fn new_pack_unpack_roundtrip() {
    let root = temp_root("roundtrip");
    let dir = skeleton(&root, "demo");
    let script = fs::read_to_string(dir.join("index.js")).expect("读脚本");
    assert!(script.contains("lueying.register"), "骨架脚本要有注册入口");

    let out = pack(&PackArgs {
        dir: dir.clone(),
        out: None,
    })
    .expect("pack 应成功");
    assert_eq!(out, root.join("demo.luext"));
    let text = fs::read_to_string(&out).expect("读包");
    assert!(text.starts_with(PACK_HEADER), "包要以 lueying-pack 头开始");

    let (id, files) = unpack(&text).expect("unpack 应成功");
    assert_eq!(id, "demo");
    let manifest = files
        .iter()
        .find(|(name, _)| name == "manifest.json")
        .expect("包里要有清单");
    assert_eq!(
        manifest.1,
        fs::read_to_string(dir.join("manifest.json")).expect("读清单")
    );
    let packed_script = files
        .iter()
        .find(|(name, _)| name == "index.js")
        .expect("包里要有脚本");
    assert_eq!(packed_script.1, script);
}

#[test]
fn pack_roundtrips_blank_lines_and_subdirs() {
    let root = temp_root("blank");
    let dir = skeleton(&root, "demo");
    fs::create_dir_all(dir.join("sub")).expect("建子目录");
    let notes = "第一行\n\n第三行\n";
    fs::write(dir.join("sub").join("notes.md"), notes).expect("写子目录文件");

    let out = pack(&PackArgs {
        dir: dir.clone(),
        out: None,
    })
    .expect("pack 应成功");
    let (_, files) = unpack(&fs::read_to_string(out).expect("读包")).expect("unpack 应成功");
    let found = files
        .iter()
        .find(|(name, _)| name == "sub/notes.md")
        .expect("子目录文件要进包");
    assert_eq!(found.1, notes, "空行要原样保留");
}

#[test]
fn new_rejects_bad_id_and_existing_dir() {
    let root = temp_root("bad-new");
    let bad = new(&NewArgs {
        id: "Bad!".to_string(),
        name: None,
        dir: Some(root.clone()),
    });
    assert!(bad.is_err(), "非法 id 要被拒绝");

    let dir = skeleton(&root, "demo");
    assert!(dir.join("manifest.json").exists());
    let again = new(&NewArgs {
        id: "demo".to_string(),
        name: None,
        dir: Some(root),
    });
    assert!(again.is_err(), "已有目录要被拒绝");
}

#[test]
fn install_refuses_overwrite_then_force() {
    let root = temp_root("install");
    let dir = skeleton(&root, "demo");
    let pack_path = pack(&PackArgs { dir, out: None }).expect("pack 应成功");
    let out_root = root.join("out");

    let installed = install(&InstallArgs {
        source: pack_path.clone(),
        dir: Some(out_root.clone()),
        force: false,
    })
    .expect("首次安装应成功");
    assert_eq!(installed, out_root.join("demo"));
    assert!(installed.join("index.js").exists());

    let second = install(&InstallArgs {
        source: pack_path.clone(),
        dir: Some(out_root.clone()),
        force: false,
    });
    assert!(second.is_err(), "不覆盖已有包");

    let third = install(&InstallArgs {
        source: pack_path,
        dir: Some(out_root),
        force: true,
    })
    .expect("force 覆盖应成功");
    assert_eq!(third, out_root.join("demo"));
}

#[test]
fn pack_rejects_dir_id_mismatch() {
    let root = temp_root("mismatch");
    let dir = root.join("wrong");
    fs::create_dir_all(&dir).expect("建目录");
    fs::write(
        dir.join("manifest.json"),
        r#"{"id":"demo","name":"示例","version":"0.1.0","api":2,"main":"index.js"}"#,
    )
    .expect("写清单");
    let error = pack(&PackArgs { dir, out: None }).expect_err("目录名与 id 不一致要报错");
    assert!(error.contains("不一致"), "{error}");
}

#[test]
fn pack_rejects_non_utf8() {
    let root = temp_root("utf8");
    let dir = skeleton(&root, "demo");
    fs::write(dir.join("data.js"), [0xFF_u8, 0xFE]).expect("写二进制");
    let error = pack(&PackArgs { dir, out: None }).expect_err("二进制要被拒绝");
    assert!(error.contains("UTF-8"), "{error}");
}

#[test]
fn unpack_rejects_bad_header_and_unsafe_paths() {
    assert!(unpack("没有头").is_err(), "缺头要被拒绝");

    let evil = "### lueying-pack v1\n### begin: ../evil.js 1\nx\n### end: ../evil.js\n";
    let error = unpack(evil).expect_err("越界路径要被拒绝");
    assert!(error.contains("不安全"), "{error}");

    let no_manifest = "### lueying-pack v1\n### begin: a.js 1\nx\n### end: a.js\n";
    let error = unpack(no_manifest).expect_err("缺清单要被拒绝");
    assert!(error.contains("manifest.json"), "{error}");

    let short = "### lueying-pack v1\n### begin: a.js 3\n只有一行\n";
    let error = unpack(short).expect_err("块被截断要被拒绝");
    assert!(error.contains("提前结束"), "{error}");
}

#[test]
fn logs_tails_last_lines() {
    let root = temp_root("logs");
    let file = root.join("server.log");
    let text: String = (0..120).map(|i| format!("line {i}\n")).collect();
    fs::write(&file, &text).expect("写日志");

    let tail = logs(&LogsArgs {
        file: Some(file.clone()),
        tail: 10,
    })
    .expect("logs 应成功");
    let expect: Vec<String> = (110..120).map(|i| format!("line {i}")).collect();
    assert_eq!(tail, expect.join("\n"));

    let whole = logs(&LogsArgs {
        file: Some(file),
        tail: 500,
    })
    .expect("logs 应成功");
    assert_eq!(whole.lines().count(), 120, "tail 超过总行数时全量返回");
}
