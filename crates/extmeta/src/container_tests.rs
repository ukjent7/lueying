use super::*;

fn sample() -> Files {
    // 文本块按规范补尾换行;样例自带尾换行才能逐字节 roundtrip。
    let mut manifest =
        r#"{"id":"demo","name":"演示","version":"0.1.0","api": 4,"wasm":"demo.wasm"}"#
            .as_bytes()
            .to_vec();
    manifest.push(b'\n');
    vec![
        ("manifest.json".into(), manifest),
        ("demo.wasm".into(), vec![0, 1, 2, 3, 250, 251]),
        (
            "docs/说明.md".into(),
            "# 标题\n\n正文\n".as_bytes().to_vec(),
        ),
    ]
}

#[test]
fn round_trips_files() {
    let packed = encode(&sample()).unwrap();
    // 二进制块原样内嵌,整包不保证 UTF-8;只看可读的控制行。
    let text = String::from_utf8_lossy(&packed);
    assert!(text.starts_with("### lueying-pack v2\n"));
    assert!(text.contains("### begin: manifest.json 1\n"));
    assert!(text.contains("### binary: demo.wasm 6\n"));
    assert_eq!(decode(&packed).unwrap(), sample());
}

#[test]
fn keeps_blank_lines_and_empty_files() {
    let files: Files = vec![
        ("empty.txt".into(), Vec::new()),
        ("gaps.txt".into(), "第一行\n\n第三行\n".as_bytes().to_vec()),
    ];
    assert_eq!(decode(&encode(&files).unwrap()).unwrap(), files);
}

#[test]
fn rejects_bad_header_and_truncation() {
    let packed = encode(&sample()).unwrap();
    assert!(decode(b"### lueying-pack v1\n").is_err());
    assert!(decode(b"").is_err());
    for cut in [0usize, 5, 15, 20, packed.len() - 1] {
        assert!(decode(&packed[..cut]).is_err(), "截断到 {cut}");
    }
    // 未知控制行:整体拒绝,宁可失败也不静默丢文件。
    let mut junk = packed.clone();
    junk.truncate(packed.len() - 1);
    junk.extend_from_slice(b"\n### stray\n");
    assert!(decode(&junk).is_err());
}

#[test]
fn rejects_unsafe_and_duplicate_names() {
    for name in [
        "",
        "consent.json",
        "/abs.txt",
        "a\\b.txt",
        "../escape.txt",
        "dir/.hidden.txt",
        "dir//x.txt",
    ] {
        let files: Files = vec![(name.to_string(), b"x".to_vec())];
        assert!(encode(&files).is_err(), "{name}");
        let mut packed = format!("{PACK_HEADER}\n").into_bytes();
        packed.extend_from_slice(format!("### binary: {name} 1\n").as_bytes());
        packed.extend_from_slice(b"x\n");
        assert!(decode(&packed).is_err(), "{name}");
    }
    let dup: Files = vec![
        ("a.txt".into(), b"1".to_vec()),
        ("a.txt".into(), b"2".to_vec()),
    ];
    assert!(encode(&dup).is_err());
    // 手工构造同名块,decode 侧同样拒绝。
    let mut packed = format!("{PACK_HEADER}\n").into_bytes();
    for body in ["1", "2"] {
        packed.extend_from_slice(b"### begin: a.txt 1\n");
        packed.extend_from_slice(body.as_bytes());
        packed.push(b'\n');
        packed.extend_from_slice(b"### end: a.txt\n");
    }
    assert!(decode(&packed).is_err());
}

#[test]
fn rejects_non_utf8_text_and_broken_binary_block() {
    let files: Files = vec![("note.txt".into(), vec![0xff, 0xfe])];
    assert!(encode(&files).is_err());
    // 字节块缺尾换行。
    let mut packed = format!("{PACK_HEADER}\n").into_bytes();
    packed.extend_from_slice(b"### binary: demo.wasm 2\n");
    packed.extend_from_slice(b"ab");
    assert!(decode(&packed).is_err());
    // end 行对不上。
    let mut packed = format!("{PACK_HEADER}\n").into_bytes();
    packed.extend_from_slice(b"### begin: a.txt 1\n");
    packed.extend_from_slice(b"x\n### end: b.txt\n");
    assert!(decode(&packed).is_err());
}
