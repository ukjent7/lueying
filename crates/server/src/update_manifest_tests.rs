//! 更新清单的回归:字段校验、内置公钥验签与已知答案向量。
//!
//! 向量由仓库外的发布种子(node WebCrypto 同款 Ed25519)签出;清单字节
//! 一字不差地嵌在这里,防止常量与签名错位。

use super::{MANIFEST_NAME, MAX_MANIFEST_BYTES};

const SAMPLE: &str = r#"{
  "version": 1,
  "release": "0.2.0",
  "notes": "示例清单",
  "files": [
    {
      "name": "lueying-0.2.0-setup.exe",
      "size": 12345678,
      "sha256": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
      "mirrors": [
        "https://github.com/ukjent7/lueying/releases/download/v0.2.0/lueying-0.2.0-setup.exe",
        "https://mirror.example.com/lueying/lueying-0.2.0-setup.exe"
      ]
    }
  ]
}
"#;

/// 已知答案:发布种子对 `SAMPLE` 字节原文的 Ed25519 签名。
const SAMPLE_SIG: &str = "6d3907111ec06cbed18efd855a6927b13096ac1d19281a9719bd94be17b449b9\
2ffa44939b0b43eb817cf7797bcfc2357da81372131a9e64411d81ea9b819808";

#[test]
fn verifies_known_answer_vector() {
    assert_eq!(SAMPLE.len(), 438, "向量字节必须与签名时一致");
    let manifest = verify(SAMPLE.as_bytes(), SAMPLE_SIG).expect("内置公钥必须验过发布种子的签名");
    assert_eq!(manifest.release, "0.2.0");
    assert_eq!(manifest.version, 1);
    let installer = manifest.installer("0.2.0").expect("安装包条目");
    assert_eq!(installer.sha256.len(), 64);
    assert_eq!(
        installer.mirror().expect("镜像"),
        "https://github.com/ukjent7/lueying/releases/download/v0.2.0/lueying-0.2.0-setup.exe"
    );
    assert!(
        installer.matches("LUEYING-0.2.0-SETUP.EXE"),
        "匹配忽略大小写"
    );
}

#[test]
fn rejects_tampered_manifest() {
    let tampered = SAMPLE.replace("0.2.0", "0.2.1");
    assert_ne!(tampered, SAMPLE);
    let error = verify(tampered.as_bytes(), SAMPLE_SIG).expect_err("改过字节的清单必须验不过");
    assert!(error.contains("签名验证失败"), "{error}");
}

#[test]
fn rejects_bad_signature_text() {
    let short = verify(SAMPLE.as_bytes(), "abcd");
    assert!(short.is_err(), "长度不对的签名要被拒绝");
    let junk = "g".repeat(128);
    let error = verify(SAMPLE.as_bytes(), &junk).expect_err("非十六进制要被拒绝");
    assert!(error.contains("十六进制"), "{error}");
    let zeros = "0".repeat(128);
    let error = verify(SAMPLE.as_bytes(), &zeros).expect_err("全零签名必须验不过");
    assert!(error.contains("签名验证失败"), "{error}");
}

#[test]
fn parses_rejects_bad_fields() {
    let parse = |text: &str| parse(text.as_bytes());
    assert!(
        parse(r#"{"version":2,"release":"0.2.0","files":[]}"#).is_err(),
        "版本要校验"
    );
    assert!(
        parse(r#"{"version":1,"release":"","files":[]}"#)
            .err()
            .is_some_and(|error| error.contains("release")),
        "空 release 要报原因"
    );
    assert!(
        parse(r#"{"version":1,"release":"0.2.0","files":[]}"#)
            .err()
            .is_some_and(|error| error.contains("没有文件")),
        "空 files 要报原因"
    );
    let dup = r#"{"version":1,"release":"0.2.0","files":[
        {"name":"a.exe","size":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","mirrors":["https://a/x"]},
        {"name":"A.EXE","size":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","mirrors":["https://a/x"]}]}"#;
    let error = parse(dup).expect_err("重名要被拒绝");
    assert!(error.contains("重复声明"), "{error}");
    let path = r#"{"version":1,"release":"0.2.0","files":[
        {"name":"../a.exe","size":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","mirrors":["https://a/x"]}]}"#;
    assert!(parse(path).is_err(), "路径分隔与越界名要被拒绝");
    let http = r#"{"version":1,"release":"0.2.0","files":[
        {"name":"a.exe","size":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","mirrors":["http://a/x"]}]}"#;
    let error = parse(http).expect_err("非 https 镜像要被拒绝");
    assert!(error.contains("https"), "{error}");
    let sha = r#"{"version":1,"release":"0.2.0","files":[
        {"name":"a.exe","size":1,"sha256":"abc","mirrors":["https://a/x"]}]}"#;
    let error = parse(sha).expect_err("短哈希要被拒绝");
    assert!(error.contains("十六进制"), "{error}");
    let no_mirror = r#"{"version":1,"release":"0.2.0","files":[
        {"name":"a.exe","size":1,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","mirrors":[]}]}"#;
    let error = parse(no_mirror).expect_err("空镜像要被拒绝");
    assert!(error.contains("镜像"), "{error}");
}

#[test]
fn installer_reports_missing_entry() {
    let manifest = parse(SAMPLE.as_bytes()).expect("样例可解析");
    let error = manifest
        .installer("9.9.9")
        .expect_err("版本对不上要报可读原因");
    assert!(error.contains("lueying-9.9.9-setup.exe"), "{error}");
}

#[test]
fn parse_rejects_oversize() {
    let big = vec![b'{'; MAX_MANIFEST_BYTES + 1];
    let error = parse(&big).expect_err("超大清单要被拒绝");
    assert!(error.contains("超过"), "{error}");
}

#[test]
fn manifest_name_matches_release_asset() {
    assert_eq!(MANIFEST_NAME, "lueying-manifest.json");
}
