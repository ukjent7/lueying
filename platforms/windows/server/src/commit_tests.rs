//! 门禁矩阵与令牌文件往返;随 server lib 测试在 Linux 与 Windows 都跑。

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::commit::{TOKEN_BYTES, decide, ensure_token, generate_token, token_path};

const TOKEN: &str = "a6f1c2e4b8d30417a6f1c2e4b8d30417a6f1c2e4b8d30417a6f1c2e4b8d30417";

#[test]
fn closed_channel_denies_everything() {
    let error = decide(false, Some(TOKEN), TOKEN, false).expect_err("关闭即拒");
    assert!(error.contains("未开启"));
}

#[test]
fn missing_token_denies() {
    let error = decide(true, None, TOKEN, false).expect_err("无令牌即拒");
    assert!(error.contains("令牌"));
}

#[test]
fn wrong_token_denies() {
    let error = decide(true, Some(TOKEN), "ffffff", false).expect_err("令牌不符即拒");
    assert!(error.contains("不匹配"));
}

#[test]
fn private_field_denies_even_with_valid_token() {
    let error = decide(true, Some(TOKEN), TOKEN, true).expect_err("隐私字段即拒");
    assert!(error.contains("隐私"));
}

#[test]
fn valid_call_passes_all_gates() {
    assert!(decide(true, Some(TOKEN), TOKEN, false).is_ok());
}

#[test]
fn token_format_is_hex64() {
    let token = generate_token().expect("令牌应能生成");
    assert_eq!(token.len(), TOKEN_BYTES * 2);
    assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    // 两次生成不同:同进程内连发也几乎不可能撞上。
    assert_ne!(token, generate_token().expect("令牌应能生成"));
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "lueying-commit-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("临时目录应能创建");
    dir
}

#[test]
fn token_file_round_trip_and_persistence() {
    let dir = temp_dir("roundtrip");
    let path = token_path(&dir);
    let first = ensure_token(&path).expect("首次应生成");
    assert_eq!(first.len(), TOKEN_BYTES * 2);
    assert!(path.is_file());
    // 第二次读同一份;文件改坏后重新生成,不把坏值当令牌。
    let second = ensure_token(&path).expect("再次应读取");
    assert_eq!(first, second);
    fs::write(&path, "").expect("清空文件");
    let regenerated = ensure_token(&path).expect("空文件后应重生成");
    assert_ne!(first, regenerated);
    let _ = fs::remove_dir_all(&dir);
}
