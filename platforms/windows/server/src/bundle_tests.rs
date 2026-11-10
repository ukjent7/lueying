use super::*;

#[test]
fn pack_and_unpack_round_trip() {
    let blocks = vec![
        (
            "config.toml".to_string(),
            "scheme = \"full\"\npage_size = 5".to_string(),
        ),
        ("phrases.txt".to_string(), "你好,世界".to_string()),
        ("empty.txt".to_string(), String::new()),
    ];
    let text = pack(&blocks);
    assert!(is_bundle(&text), "{text}");
    let parsed = unpack(&text);
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].0, "config.toml");
    assert_eq!(parsed[0].1, "scheme = \"full\"\npage_size = 5");
    assert_eq!(parsed[1].1, "你好,世界");
    assert_eq!(parsed[2].1, "");
}

#[test]
fn pack_keeps_markers_inside_content() {
    let tricky = "### begin: fake 1\n### end: fake\n";
    let text = pack(&[("punct.txt".to_string(), tricky.to_string())]);
    let parsed = unpack(&text);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].1, tricky.trim_end_matches('\n'));
}

#[test]
fn unpack_skips_broken_blocks() {
    let text = "### lueying-backup v1\n### begin: bad\n### begin: ok 1\n内容\n### end: ok\n";
    let parsed = unpack(text);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].0, "ok");
    assert_eq!(parsed[0].1, "内容");
}
