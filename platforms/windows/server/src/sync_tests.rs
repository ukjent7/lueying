use super::*;
use std::path::PathBuf;

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lueying-sync-unit-{}-{name}", std::process::id()))
}

#[test]
fn pack_and_unpack_round_trip() {
    let blocks = vec![
        ("config.toml".to_string(), "scheme = \"full\"\npage_size = 5".to_string()),
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

#[test]
fn digest_separates_different_payloads() {
    let one = pack(&[("a.txt".to_string(), "一".to_string())]);
    let same = pack(&[("a.txt".to_string(), "一".to_string())]);
    let other = pack(&[("a.txt".to_string(), "二".to_string())]);
    assert_eq!(digest(&one), digest(&same));
    assert_ne!(digest(&one), digest(&other));
    assert_eq!(digest(&one).len(), 64);
}

#[test]
fn decide_pushes_when_remote_is_missing() {
    let local = "本机";
    assert_eq!(decide(local, None, None), Action::Push);
    assert_eq!(decide(local, None, Some("别的")), Action::Push);
}

#[test]
fn decide_reports_up_to_date_for_equal_payloads() {
    assert_eq!(decide("同一份", Some("同一份"), None), Action::UpToDate);
}

#[test]
fn decide_follows_the_side_that_changed() {
    let base = "上次版本";
    let last = digest(base);
    assert_eq!(
        decide("本机版本", Some("远端版本"), Some(&last)),
        Action::Conflict
    );
    assert_eq!(
        decide(base, Some("远端版本"), Some(&last)),
        Action::Pull,
        "本机没动,远端动了"
    );
    assert_eq!(
        decide("本机版本", Some(base), Some(&last)),
        Action::Push,
        "远端没动,本机动了"
    );
}

#[test]
fn decide_without_history_conflicts() {
    assert_eq!(decide("本机", Some("远端"), None), Action::Conflict);
}

#[test]
fn remote_url_joins_the_file_name() {
    assert_eq!(
        remote_url("https://dav.example.com/ly"),
        "https://dav.example.com/ly/lueying-config.txt"
    );
    assert_eq!(
        remote_url("https://dav.example.com/ly/"),
        "https://dav.example.com/ly/lueying-config.txt"
    );
    assert_eq!(
        remote_url("  https://dav.example.com/ly  "),
        "https://dav.example.com/ly/lueying-config.txt"
    );
}

#[test]
fn valid_url_only_allows_https_outside_loopback() {
    assert!(valid_url(""));
    assert!(valid_url("https://dav.example.com/ly"));
    assert!(valid_url("http://localhost:8080/ly"));
    assert!(valid_url("http://127.0.0.1/ly"));
    assert!(valid_url("http://[::1]/ly"));
    assert!(!valid_url("http://dav.example.com/ly"));
    assert!(!valid_url("ftp://dav.example.com/ly"));
    assert!(!valid_url("dav.example.com"));
    assert!(!valid_url("https://"));
}

#[test]
fn write_atomic_replaces_the_file() {
    let path = temp("atomic.txt");
    let _ = std::fs::remove_file(&path);
    write_atomic(&path, "第一份").expect("写第一份");
    write_atomic(&path, "第二份").expect("写第二份");
    let text = std::fs::read_to_string(&path).expect("读回");
    let _ = std::fs::remove_file(&path);
    assert_eq!(text, "第二份");
}
