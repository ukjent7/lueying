use super::*;

fn sample() -> Vec<Entry> {
    vec![
        Entry {
            name: "manifest.json".into(),
            bytes: br#"{"id":"demo","name":"演示","version":"0.1.0","api":3,"wasm":"demo.wasm"}"#
                .to_vec(),
        },
        Entry {
            name: "demo.wasm".into(),
            bytes: vec![0, 1, 2, 3, 250, 251],
        },
        Entry {
            name: "场景.toml".into(),
            bytes: "scene = true".as_bytes().to_vec(),
        },
    ]
}

#[test]
fn round_trips_entries() {
    let packed = pack(&sample()).unwrap();
    assert_eq!(&packed[..5], b"LUEXT");
    assert_eq!(unpack(&packed).unwrap(), sample());
}

#[test]
fn rejects_bad_magic_and_version() {
    let mut packed = pack(&sample()).unwrap();
    packed[0] = b'X';
    assert!(unpack(&packed).is_err());
    let mut packed = pack(&sample()).unwrap();
    packed[5] = 9;
    assert!(unpack(&packed).is_err());
    assert!(unpack(b"LUEXT").is_err());
}

#[test]
fn rejects_bad_names() {
    for name in [
        "",
        "consent.json",
        "a/b",
        r"a\b",
        ".hidden",
        "manifest.json/..",
        &"x".repeat(MAX_NAME + 1),
    ] {
        let entry = Entry {
            name: name.to_string(),
            bytes: vec![1],
        };
        assert!(pack(&[entry]).is_err(), "{name}");
    }
}

#[test]
fn rejects_bad_counts_and_truncation() {
    assert!(pack(&[]).is_err());
    let many: Vec<Entry> = (0..=MAX_FILES)
        .map(|index| Entry {
            name: format!("file{index}.txt"),
            bytes: vec![1],
        })
        .collect();
    assert!(pack(&many).is_err());

    let packed = pack(&sample()).unwrap();
    for cut in [0usize, 5, 9, 12, packed.len() - 1] {
        assert!(unpack(&packed[..cut]).is_err(), "截断到 {cut}");
    }
}

#[test]
fn rejects_duplicate_names() {
    let entries = vec![
        Entry {
            name: "a.txt".into(),
            bytes: vec![1],
        },
        Entry {
            name: "a.txt".into(),
            bytes: vec![2],
        },
    ];
    assert!(pack(&entries).is_err());
    // 手工构造两个同名条目的合法字节流,解包侧同样拒绝。
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"LUEXT");
    bytes.push(1);
    bytes.extend_from_slice(&2u32.to_le_bytes());
    for content in [[1u8], [2]] {
        bytes.extend_from_slice(&5u16.to_le_bytes());
        bytes.extend_from_slice(b"a.txt");
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&content);
    }
    assert!(unpack(&bytes).is_err());
}
