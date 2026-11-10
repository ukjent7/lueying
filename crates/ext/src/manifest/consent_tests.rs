use std::path::{Path, PathBuf};

use super::*;

const MANIFEST: &str = r#"{
  "id":"demo","name":"演示","version":"1.0.0","api": 4,
  "wasm":"extension.wasm",
  "activate":{"commands":[{"id":"run","title":"运行"}]},
  "needs":["capture","output"],
  "capabilities":["screen-capture"]
}"#;

const APPEARANCE: &str = r#"{
  "id":"look","name":"外观","version":"1.0.0","api": 4,"wasm":"extension.wasm"
}"#;

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

fn write_pack(dir: &Path, manifest: &str, wasm: &[u8]) {
    std::fs::write(dir.join("manifest.json"), manifest).expect("写清单");
    std::fs::write(dir.join("extension.wasm"), wasm).expect("写模块");
}

fn parse(manifest: &str) -> Manifest {
    Manifest::parse(manifest.as_bytes()).expect("清单应合法")
}

#[test]
fn appearance_only_needs_no_consent() {
    let temp = Temp::new("appearance");
    let dir = temp.pack("look");
    write_pack(&dir, APPEARANCE, b"wasm");
    let check = ConsentBook::default()
        .check(&dir, &parse(APPEARANCE))
        .expect("检查");
    assert!(!check.required);
    assert!(check.approved);
    assert!(check.items.is_empty());
}

#[test]
fn hash_pins_manifest_and_wasm() {
    let temp = Temp::new("pin");
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST, b"v1");
    let manifest = parse(MANIFEST);
    let mut book = ConsentBook::default();
    let missing = book.check(&dir, &manifest).expect("检查");
    assert!(missing.required && !missing.approved && !missing.changed);

    book.approve(&dir, &manifest, 42).expect("同意");
    assert!(book.check(&dir, &manifest).expect("检查").approved);

    // 包里的数据文件不参与指纹,加一个不会要求重新同意。
    std::fs::write(dir.join("data.txt"), "x").expect("写数据文件");
    assert!(book.check(&dir, &manifest).expect("检查").approved);

    std::fs::write(dir.join("extension.wasm"), b"v2").expect("换模块");
    let changed = book.check(&dir, &manifest).expect("检查");
    assert!(changed.changed && !changed.approved);

    book.approve(&dir, &manifest, 43).expect("重新同意");
    assert!(book.check(&dir, &manifest).expect("检查").approved);
}

#[test]
fn upgrade_reports_added_capabilities() {
    let temp = Temp::new("upgrade");
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST, b"wasm");
    let manifest = parse(MANIFEST);
    let mut book = ConsentBook::default();
    book.approve(&dir, &manifest, 42).expect("同意");

    let upgraded = MANIFEST
        .replace(
            r#""capabilities":["screen-capture"]"#,
            r#""capabilities":["screen-capture","clipboard"]"#,
        )
        .replace(
            r#""needs":["capture","output"]"#,
            r#""needs":["capture","output","clipboard"]"#,
        );
    write_pack(&dir, &upgraded, b"wasm");
    let check = book.check(&dir, &parse(&upgraded)).expect("检查");
    assert!(check.changed && !check.approved);
    assert!(check.added.iter().any(|item| item.contains("剪贴板")));
}

#[test]
fn save_load_roundtrip() {
    let temp = Temp::new("roundtrip");
    let dir = temp.pack("demo");
    write_pack(&dir, MANIFEST, b"wasm");
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
    write_pack(&dir, MANIFEST, b"wasm");
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
fn package_hash_requires_wasm() {
    let temp = Temp::new("no-wasm");
    let dir = temp.pack("demo");
    std::fs::write(dir.join("manifest.json"), MANIFEST).expect("写清单");
    let error = package_hash(&dir, &parse(MANIFEST)).expect_err("缺模块应失败");
    assert!(error.contains("extension.wasm"));
}

const NATIVE: &str = r#"{
  "id":"ppocr","name":"PP-OCR","version":"1.0.0","api": 4,
  "kind":"native","exe":"ppocr.exe","capabilities":["ocr"]
}"#;

#[test]
fn native_hash_covers_exe_bytes() {
    let temp = Temp::new("native-hash");
    let dir = temp.pack("ppocr");
    std::fs::write(dir.join("manifest.json"), NATIVE).expect("写清单");
    std::fs::write(dir.join("ppocr.exe"), b"v1").expect("写可执行文件");
    let manifest = parse(NATIVE);
    let mut book = ConsentBook::default();
    book.approve(&dir, &manifest, 42).expect("同意");
    assert!(book.check(&dir, &manifest).expect("检查").approved);

    std::fs::write(dir.join("ppocr.exe"), b"v2").expect("换可执行文件");
    let check = book.check(&dir, &manifest).expect("检查");
    assert!(check.changed && !check.approved, "exe 变了要重新同意");
}

const NETWORK: &str = r#"{
  "id":"net","name":"联网","version":"1.0.0","api": 4,"wasm":"extension.wasm",
  "needs":["network"],"network":{"allow":["api.example.com","dev.local:8080"]}
}"#;

#[test]
fn private_approval_follows_package_content() {
    let temp = Temp::new("private");
    let dir = temp.pack("net");
    write_pack(&dir, NETWORK, b"v1");
    let manifest = parse(NETWORK);
    let mut book = ConsentBook::default();
    assert!(!book.private_ok(&dir, &manifest), "未同意时不许私密");
    book.approve(&dir, &manifest, 1).expect("同意");
    assert!(!book.private_ok(&dir, &manifest), "同意本身不等于私密许可");

    book.set_private(&dir, &manifest, true).expect("允许私密");
    assert!(book.private_ok(&dir, &manifest));

    std::fs::write(dir.join("extension.wasm"), b"v2").expect("换模块");
    assert!(!book.private_ok(&dir, &manifest), "内容变了要重新确认");
    assert!(book.set_private(&dir, &manifest, true).is_err());

    book.set_private(&dir, &manifest, false).expect("撤销");
    assert!(!book.private_ok(&dir, &manifest));
}

#[test]
fn private_approval_needs_a_consented_entry() {
    let temp = Temp::new("private-none");
    let dir = temp.pack("net");
    write_pack(&dir, NETWORK, b"v1");
    let manifest = parse(NETWORK);
    let mut book = ConsentBook::default();
    assert!(book.set_private(&dir, &manifest, true).is_err());
    assert!(book.set_private(&dir, &manifest, false).is_err());
}

#[test]
fn private_record_survives_save_and_load() {
    let temp = Temp::new("private-save");
    let dir = temp.pack("net");
    write_pack(&dir, NETWORK, b"v1");
    let manifest = parse(NETWORK);
    let path = temp.root.join("consent.json");
    let mut book = ConsentBook::default();
    book.approve(&dir, &manifest, 1).expect("同意");
    book.set_private(&dir, &manifest, true).expect("允许私密");
    book.save(&path).expect("保存");
    assert!(ConsentBook::load(&path).private_ok(&dir, &manifest));
}

#[test]
fn network_items_name_domains_and_state_native_limits() {
    let items = consent_items(&parse(NETWORK));
    assert_eq!(
        items,
        vec![
            "访问外部网络(仅限 api.example.com、dev.local:8080 及其子域名;原生扩展不受白名单约束)"
        ]
    );
    let native = consent_items(&parse(NATIVE));
    assert!(
        native
            .iter()
            .any(|item| item.contains("原生扩展不受白名单约束")),
        "{native:?}"
    );
}
