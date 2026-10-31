//! 官方扩展目录的结构约束。

use super::*;

#[test]
fn catalog_ids_are_unique_and_known() {
    let catalog = catalog();
    assert_eq!(catalog.version, 1);
    for (index, extension) in catalog.extensions.iter().enumerate() {
        assert!(
            catalog.extensions[..index]
                .iter()
                .all(|other| other.id != extension.id),
            "目录 id {} 重复",
            extension.id
        );
        assert!(extension.is_extension() || extension.is_model());
        if extension.is_extension() {
            assert!(extension.bundled.is_some(), "{} 缺随包来源", extension.id);
        }
        if extension.is_model() {
            assert!(!extension.files.is_empty(), "{} 没有下载文件", extension.id);
        }
    }
    assert!(find("shot-ocr").is_some());
    assert!(find("ppocr").is_some());
    assert!(find("rerank-25m").is_some());
}

#[test]
fn every_download_has_hash_and_size() {
    for extension in &catalog().extensions {
        for file in &extension.files {
            assert_eq!(file.sha256.len(), 64, "{} 的哈希长度不对", file.name);
            assert!(file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(file.bytes > 0, "{} 缺字节数", file.name);
            assert!(
                file.url.starts_with("https://"),
                "{} 不是 HTTPS 地址",
                file.name
            );
        }
    }
}

#[test]
fn ppocr_dict_is_complete() {
    let lines = PPOCR_DICT.lines().count();
    assert_eq!(lines, 18_708, "PP-OCR 词表行数变了,模型输出会错位");
}
