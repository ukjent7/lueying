//! 皮肤解析的边界:缺文件、坏键、坏值、空表与取表规则。

use std::fs;
use std::path::{Path, PathBuf};

use super::{Skin, SkinColors};

fn temp_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir =
        std::env::temp_dir().join(format!("lueying-skin-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("临时目录应能创建");
    dir.join("skin.toml")
}

#[test]
fn missing_file_is_default_without_issues() {
    let (skin, issues) = Skin::load(Path::new("/nonexistent/lueying/skin.toml"));
    assert!(skin.is_empty());
    assert!(skin.light.is_none() && skin.dark.is_none());
    assert!(issues.is_empty());
}

#[test]
fn parses_light_and_dark_tables() {
    let path = temp_path("parse");
    fs::write(
        &path,
        "[light]\nbg = 0xfafafa\naccent = 0x0a7d32\n\n[dark]\nsurface = 0x101418\n",
    )
    .expect("写皮肤");
    let (skin, issues) = Skin::load(&path);
    assert!(issues.is_empty());
    let light = skin.light.expect("浅色表应存在");
    assert_eq!(light.bg, Some(0xfafafa));
    assert_eq!(light.accent, Some(0x0a7d32));
    assert_eq!(light.surface, None);
    let dark = skin.dark.expect("深色表应存在");
    assert_eq!(dark.surface, Some(0x101418));
    let _ = fs::remove_dir_all(path.parent().expect("有父目录"));
}

#[test]
fn unknown_keys_reject_the_file() {
    let path = temp_path("unknown");
    fs::write(&path, "[light]\nbg = 0xfafafa\nwidth = 480\n").expect("写皮肤");
    let (skin, issues) = Skin::load(&path);
    assert!(skin.is_empty(), "坏文件整份回落默认");
    assert!(issues.iter().any(|issue| issue.contains("解析失败")));
    let _ = fs::remove_dir_all(path.parent().expect("有父目录"));
}

#[test]
fn out_of_range_value_is_dropped_but_rest_survives() {
    let path = temp_path("range");
    fs::write(
        &path,
        "[light]\nbg = 0x1234567\naccent = 0x0a7d32\n[dark]\ntext = 0x1000000\n",
    )
    .expect("写皮肤");
    let (skin, issues) = Skin::load(&path);
    assert_eq!(issues.len(), 2, "两张表各记一条坏值");
    let light = skin.light.expect("浅色表应保留");
    assert_eq!(light.bg, None, "坏值丢掉");
    assert_eq!(light.accent, Some(0x0a7d32), "其余键照常生效");
    assert!(skin.dark.is_none(), "整表都坏就整表不覆盖");
    let _ = fs::remove_dir_all(path.parent().expect("有父目录"));
}

#[test]
fn empty_table_counts_as_no_override() {
    let path = temp_path("empty");
    fs::write(&path, "[light]\n").expect("写皮肤");
    let (skin, issues) = Skin::load(&path);
    assert!(issues.is_empty());
    assert!(skin.light.is_none());
    assert!(skin.is_empty());
    let _ = fs::remove_dir_all(path.parent().expect("有父目录"));
}

#[test]
fn appearance_picks_the_table() {
    let skin = Skin {
        light: Some(SkinColors {
            bg: Some(1),
            ..SkinColors::default()
        }),
        dark: Some(SkinColors {
            bg: Some(2),
            ..SkinColors::default()
        }),
    };
    assert_eq!(skin.for_appearance(false).and_then(|c| c.bg), Some(1));
    assert_eq!(skin.for_appearance(true).and_then(|c| c.bg), Some(2));
}
