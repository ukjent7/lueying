//! `ext add` 的纯逻辑回归:来源解析、Release 资产挑选、扩展目录定位与
//! 解包路径护栏。联网路径不进单测。

use std::fs;
use std::path::{Path, PathBuf};

use super::{parse_source, pick_asset, pick_extension_dir, safe_relative};

fn temp_root(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("lueying-extadd-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("建临时目录");
    base
}

fn manifest_json(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"{id}","version":"1.0.0","api":4,"wasm":"extension.wasm"}}"#)
}

#[test]
fn parses_github_sources() {
    assert_eq!(
        parse_source("github:alice/repo#v1.2.3"),
        Ok(("alice", "repo", "v1.2.3"))
    );
    assert_eq!(
        parse_source("github:al.re-p_1/do.re_1#1_2-x"),
        Ok(("al.re-p_1", "do.re_1", "1_2-x"))
    );
    assert_eq!(
        parse_source("github:alice/repo#9c1f8e2d"),
        Ok(("alice", "repo", "9c1f8e2d"))
    );
    assert_eq!(
        parse_source("github:alice/repo # v1 "),
        Ok(("alice", "repo", "v1"))
    );
}

#[test]
fn rejects_bad_sources() {
    for source in [
        "https://github.com/alice/repo#v1",
        "github:alice/repo",
        "github:alice/repo#",
        "github:alice/repo# ",
        "github:alice#v1",
        "github:alice/repo/extra#v1",
        "github:/repo#v1",
        "github:alice/#v1",
        "github:alice/repo#v1/x",
        "github:alice/repo#..",
        "github:alice/repo#a b",
        "",
    ] {
        assert!(parse_source(source).is_err(), "{source:?} 应当被拒");
    }
}

fn release_with(names: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "tag_name": "v1",
        "assets": names
            .iter()
            .enumerate()
            .map(|(index, name)| serde_json::json!({
                "name": name,
                "browser_download_url": format!("https://example.com/{index}"),
                "size": 1,
            }))
            .collect::<Vec<_>>(),
    })
}

#[test]
fn picks_single_luext_asset() {
    let release = release_with(&["readme.txt", "repo.luext"]);
    let asset = pick_asset("repo", &release)
        .expect("应挑中")
        .expect("应有资产");
    assert_eq!(asset.0, "https://example.com/1");
}

#[test]
fn prefers_repo_named_asset_when_many() {
    let release = release_with(&["other.luext", "Repo.LUEXT"]);
    let asset = pick_asset("repo", &release)
        .expect("应挑中")
        .expect("应有资产");
    assert_eq!(asset.0, "https://example.com/1");
}

#[test]
fn rejects_ambiguous_assets() {
    let release = release_with(&["one.luext", "two.luext"]);
    let error = pick_asset("repo", &release).err().expect("应当拒绝");
    assert!(
        error.contains("one.luext") && error.contains("two.luext"),
        "{error}"
    );
}

#[test]
fn no_asset_means_none() {
    let release = release_with(&["readme.txt"]);
    assert!(pick_asset("repo", &release).expect("应答合法").is_none());
}

#[test]
fn picks_root_manifest() {
    let root = temp_root("root");
    fs::write(root.join("manifest.json"), manifest_json("demo")).expect("写清单");
    assert_eq!(pick_extension_dir(&root).expect("应定位到根"), root);
}

#[test]
fn picks_single_extensions_layout() {
    let root = temp_root("layout");
    let dir = root.join("extensions/demo");
    fs::create_dir_all(&dir).expect("建目录");
    fs::write(dir.join("manifest.json"), manifest_json("demo")).expect("写清单");
    assert_eq!(
        pick_extension_dir(&root).expect("应定位到 extensions/demo"),
        dir
    );
}

#[test]
fn rejects_missing_or_ambiguous_layout() {
    let root = temp_root("missing");
    let error = pick_extension_dir(&root).err().expect("应当拒绝");
    assert!(error.contains("manifest.json"), "{error}");

    let root = temp_root("ambiguous");
    for id in ["one", "two"] {
        let dir = root.join("extensions").join(id);
        fs::create_dir_all(&dir).expect("建目录");
        fs::write(dir.join("manifest.json"), manifest_json(id)).expect("写清单");
    }
    let error = pick_extension_dir(&root).err().expect("应当拒绝");
    assert!(error.contains("one、two"), "{error}");
}

#[test]
fn guards_tar_entry_paths() {
    let ok = Path::new("repo-v1/src/main.rs");
    assert_eq!(safe_relative(ok).as_deref(), Some("repo-v1/src/main.rs"));
    for bad in ["../evil", "/abs/path", "a/../../b", ""] {
        assert!(safe_relative(Path::new(bad)).is_none(), "{bad:?} 应当被拒");
    }
}

#[test]
#[cfg(windows)]
fn guards_windows_drive_paths() {
    assert!(safe_relative(Path::new("C:\\evil")).is_none());
}
