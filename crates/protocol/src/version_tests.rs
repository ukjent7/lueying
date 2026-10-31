//! 版本比较与摘要提取的纯逻辑测试;Linux 上直接跑。

use super::*;

#[test]
fn orders_release_versions() {
    assert!(is_newer("0.2.0", "0.1.9"));
    assert!(is_newer("0.2.0", "0.2.0-beta.1"));
    assert!(is_newer("0.2.0-beta.2", "0.2.0-beta.1"));
    assert!(is_newer("v1.0.0", "0.9.9"));
    assert!(!is_newer("0.2.0", "0.2.0"));
    assert!(!is_newer("0.1.0", "0.1.0"));
}

#[test]
fn reads_sha256_from_certutil_output() {
    let text = "SHA256 hash of file C:\\tmp\\lueying.zip:\n\
                d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592\n\
                CertUtil: -hashfile command completed successfully.\n";
    assert_eq!(
        parse_sha256(text).as_deref(),
        Some("d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592")
    );
}

#[test]
fn accepts_sha256sum_and_spaced_digest() {
    let hash = "D7A8FBB307D7809469CA9ABCB0082E4F8D5651E46D3CDB762D02D0BF37C9E592";
    let lower = hash.to_ascii_lowercase();
    assert_eq!(
        parse_sha256(&format!("{hash}  lueying.zip\n")).as_deref(),
        Some(lower.as_str())
    );
    let spaced = hash
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(parse_sha256(&spaced).as_deref(), Some(lower.as_str()));
}
