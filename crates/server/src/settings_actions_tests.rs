//! `.luext` 导入落盘的测试;只盖纯函数,文件对话框不进测试。

use super::*;

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lueying-import-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建根目录");
    dir
}

fn wasm_manifest(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"演示","version":"0.1.0","api":3,"wasm":"{id}.wasm"}}"#)
}

fn wasm_package(id: &str, body: &str) -> Vec<u8> {
    let entries = vec![
        lueying_extmeta::container::Entry {
            name: "manifest.json".into(),
            bytes: wasm_manifest(id).into_bytes(),
        },
        lueying_extmeta::container::Entry {
            name: format!("{id}.wasm"),
            bytes: body.as_bytes().to_vec(),
        },
    ];
    lueying_extmeta::container::pack(&entries).unwrap()
}

fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn installs_new_package() {
    let root = temp_root("new");
    let message = install_luext_bytes(&wasm_package("demo", "v1"), &root).unwrap();
    assert!(message.contains("演示"), "{message}");
    assert!(message.contains("同意"), "{message}");
    assert_eq!(
        std::fs::read_to_string(root.join("demo").join("demo.wasm")).unwrap(),
        "v1"
    );
    assert!(!root.join(".import-demo").exists());
    cleanup(&root);
}

#[test]
fn replaces_existing_package_atomically() {
    let root = temp_root("replace");
    install_luext_bytes(&wasm_package("demo", "v1"), &root).unwrap();
    install_luext_bytes(&wasm_package("demo", "v2"), &root).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("demo").join("demo.wasm")).unwrap(),
        "v2"
    );
    assert!(!root.join(".backup-demo").exists());
    assert!(!root.join(".import-demo").exists());
    cleanup(&root);
}

#[test]
fn keeps_old_package_on_bad_upload() {
    let root = temp_root("keep");
    install_luext_bytes(&wasm_package("demo", "v1"), &root).unwrap();
    let bad = wasm_package("demo", "v2");
    let mut broken = vec![b'X'];
    broken.extend_from_slice(&bad[1..]);
    assert!(install_luext_bytes(&broken, &root).is_err());
    assert_eq!(
        std::fs::read_to_string(root.join("demo").join("demo.wasm")).unwrap(),
        "v1"
    );
    cleanup(&root);
}

#[test]
fn rejects_unusable_packages() {
    let root = temp_root("reject");
    // 缺清单。
    let entries = vec![lueying_extmeta::container::Entry {
        name: "demo.wasm".into(),
        bytes: b"v1".to_vec(),
    }];
    let bytes = lueying_extmeta::container::pack(&entries).unwrap();
    assert!(install_luext_bytes(&bytes, &root).is_err());
    // 清单不合法。
    let entries = vec![lueying_extmeta::container::Entry {
        name: "manifest.json".into(),
        bytes: b"not json".to_vec(),
    }];
    let bytes = lueying_extmeta::container::pack(&entries).unwrap();
    assert!(install_luext_bytes(&bytes, &root).is_err());
    // 缺主负载。
    let entries = vec![lueying_extmeta::container::Entry {
        name: "manifest.json".into(),
        bytes: wasm_manifest("demo").into_bytes(),
    }];
    let bytes = lueying_extmeta::container::pack(&entries).unwrap();
    assert!(install_luext_bytes(&bytes, &root).is_err());
    // 同意记录不许进包(容器层已拒,这里补一层落盘侧的事实)。
    assert!(lueying_extmeta::container::check_name("consent.json").is_err());
    assert!(!root.join("demo").exists());
    cleanup(&root);
}

#[test]
fn imports_native_package() {
    let root = temp_root("native");
    let manifest = r#"{"id":"heavy","name":"重活","version":"0.1.0","api":3,"kind":"native","exe":"heavy.exe","capabilities":["ocr"]}"#;
    let entries = vec![
        lueying_extmeta::container::Entry {
            name: "manifest.json".into(),
            bytes: manifest.as_bytes().to_vec(),
        },
        lueying_extmeta::container::Entry {
            name: "heavy.exe".into(),
            bytes: b"MZ".to_vec(),
        },
    ];
    let bytes = lueying_extmeta::container::pack(&entries).unwrap();
    let message = install_luext_bytes(&bytes, &root).unwrap();
    assert!(message.contains("原生扩展"), "{message}");
    assert!(root.join("heavy").join("heavy.exe").is_file());
    cleanup(&root);
}
