//! `.luext` 导入落盘的测试;只盖纯函数,文件对话框不进测试。

use super::*;
use lueying_extmeta::container::{self, Files};

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
    let files: Files = vec![
        ("manifest.json".into(), wasm_manifest(id).into_bytes()),
        (format!("{id}.wasm"), body.as_bytes().to_vec()),
    ];
    container::encode(&files).unwrap()
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
fn keeps_subdirectories() {
    let root = temp_root("subdir");
    let files: Files = vec![
        ("manifest.json".into(), wasm_manifest("demo").into_bytes()),
        ("demo.wasm".into(), b"v1".to_vec()),
        ("docs/说明.md".into(), b"# 标题\n".to_vec()),
    ];
    install_luext_bytes(&container::encode(&files).unwrap(), &root).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("demo").join("docs").join("说明.md")).unwrap(),
        "# 标题\n"
    );
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
    let files: Files = vec![("demo.wasm".into(), b"v1".to_vec())];
    let bytes = container::encode(&files).unwrap();
    assert!(install_luext_bytes(&bytes, &root).is_err());
    // 清单不合法。
    let files: Files = vec![("manifest.json".into(), b"not json".to_vec())];
    let bytes = container::encode(&files).unwrap();
    assert!(install_luext_bytes(&bytes, &root).is_err());
    // 缺主负载。
    let files: Files = vec![("manifest.json".into(), wasm_manifest("demo").into_bytes())];
    let bytes = container::encode(&files).unwrap();
    assert!(install_luext_bytes(&bytes, &root).is_err());
    // 同意记录不许进包(容器层已拒,这里补一层落盘侧的事实)。
    assert!(container::check_name("consent.json").is_err());
    assert!(!root.join("demo").exists());
    cleanup(&root);
}

#[test]
fn imports_native_package() {
    let root = temp_root("native");
    let manifest = r#"{"id":"heavy","name":"重活","version":"0.1.0","api":3,"kind":"native","exe":"heavy.exe","capabilities":["ocr"]}"#;
    let files: Files = vec![
        ("manifest.json".into(), manifest.as_bytes().to_vec()),
        ("heavy.exe".into(), b"MZ".to_vec()),
    ];
    let bytes = container::encode(&files).unwrap();
    let message = install_luext_bytes(&bytes, &root).unwrap();
    assert!(message.contains("原生扩展"), "{message}");
    assert!(root.join("heavy").join("heavy.exe").is_file());
    assert_eq!(std::fs::read(root.join("heavy").join("heavy.exe")).unwrap(), b"MZ");
    cleanup(&root);
}
