use std::path::{Path, PathBuf};

use super::*;

const MANIFEST: &str = r#"{
  "id":"demo","name":"演示","version":"1.0.0","api":2,
  "activate":{"commands":[{"id":"run","title":"运行"}]},
  "needs":["capture","output"],
  "capabilities":["screen-capture"]
}"#;

const APPEARANCE: &str =
    r#"{"id":"look","name":"外观","version":"1.0.0","api":1,"main":"index.js"}"#;

struct Temp {
    root: PathBuf,
}

impl Temp {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root = std::env::temp_dir().join(format!(
            "lueying-consent-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("建临时目录");
        Self { root }
    }

    fn pack(&self, id: &str) -> PathBuf {
        let dir = self.root.join(id);
        std::fs::create_dir_all(&dir).expect("建包目录");
        dir
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write_pack(dir: &Path, manifest: &str) {
    std::fs::write(dir.join("manifest.json"), manifest).expect("写清单");
    std::fs::write(
        dir.join("index.js"),
        "lueying.command('run', function () {});\n",
    )
    .expect("写入口");
}

fn parse(manifest: &str) -> Manifest {
    Manifest::parse(manifest.as_bytes()).expect("清单应合法")
}

#[test]
fn appearance_only_needs_no_consent() {
    let temp = Temp::new("appearance");
    let dir = temp.pack("look");
    write_pack(&dir, APPEARANCE);
    let check = ConsentBook::default()
        .check(&dir, &parse(APPEARANCE))
        .expect("检查");
    assert!(!check.required);
    assert!(check.approved);
    assert!(check.items.is_empty());
}

#[test]
fn hash_pins_manifest_and_script() {
    let temp = Temp::new("pin");
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST);
    let manifest = parse(MANIFEST);
    let mut book = ConsentBook::default();
    let missing = book.check(&dir, &manifest).expect("检查");
    assert!(missing.required && !missing.approved && !missing.changed);

    book.approve(&dir, &manifest, 42).expect("同意");
    assert!(book.check(&dir, &manifest).expect("检查").approved);

    // 包里的数据文件不参与指纹,加一个不会要求重新同意。
    std::fs::write(dir.join("data.txt"), "x").expect("写数据文件");
    assert!(book.check(&dir, &manifest).expect("检查").approved);

    std::fs::write(
        dir.join("index.js"),
        "lueying.command('run', function () { return 1; });\n",
    )
    .expect("改入口");
    let changed = book.check(&dir, &manifest).expect("检查");
    assert!(changed.changed && !changed.approved);

    book.approve(&dir, &manifest, 43).expect("重新同意");
    assert!(book.check(&dir, &manifest).expect("检查").approved);
}

#[test]
fn upgrade_reports_added_capabilities() {
    let temp = Temp::new("upgrade");
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST);
    let manifest = parse(MANIFEST);
    let mut book = ConsentBook::default();
    book.approve(&dir, &manifest, 42).expect("同意");

    let upgraded = MANIFEST
        .replace(
            r#""capabilities":["screen-capture"]"#,
            r#""capabilities":["screen-capture","network"]"#,
        )
        .replace(
            r#""needs":["capture","output"]"#,
            r#""needs":["capture","output","net"]"#,
        );
    write_pack(&dir, &upgraded);
    let check = book.check(&dir, &parse(&upgraded)).expect("检查");
    assert!(check.changed && !check.approved);
    assert!(check.added.iter().any(|item| item.contains("访问网络")));
    assert!(check.sensitive, "读屏 + 外发要强提示");
}

#[test]
fn save_load_roundtrip() {
    let temp = Temp::new("roundtrip");
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST);
    let manifest = parse(MANIFEST);
    let mut book = ConsentBook::default();
    book.approve(&dir, &manifest, 42).expect("同意");
    let path = temp.root.join("consent.json");
    book.save(&path).expect("保存");
    let loaded = ConsentBook::load(&path);
    assert_eq!(loaded, book);
    assert!(loaded.check(&dir, &manifest).expect("检查").approved);
}

#[test]
fn corrupt_book_fails_closed() {
    let temp = Temp::new("corrupt");
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST);
    let path = temp.root.join("consent.json");
    std::fs::write(&path, b"{not json").expect("写坏文件");
    let book = ConsentBook::load(&path);
    assert!(book.extensions.is_empty());
    let check = book.check(&dir, &parse(MANIFEST)).expect("检查");
    assert!(check.required && !check.approved);
}

#[test]
fn items_merge_capability_and_service() {
    let manifest = parse(MANIFEST);
    assert_eq!(
        consent_items(&manifest),
        vec!["截取屏幕(高敏感)", "写入当前输入框"]
    );
}

#[test]
fn sensitive_needs_both_read_and_network() {
    let read_only = parse(MANIFEST);
    assert!(!consent_sensitive(&read_only));
    let outbound = MANIFEST.replace(
        r#""capabilities":["screen-capture"]"#,
        r#""capabilities":["screen-capture","network"]"#,
    );
    assert!(consent_sensitive(&parse(&outbound)));
}

#[test]
fn package_hash_requires_entry() {
    let temp = Temp::new("no-entry");
    let dir = temp.pack("demo");
    std::fs::write(dir.join("manifest.json"), MANIFEST).expect("写清单");
    let error = package_hash(&dir, &parse(MANIFEST)).expect_err("缺入口应失败");
    assert!(error.contains("index.js"));
}
