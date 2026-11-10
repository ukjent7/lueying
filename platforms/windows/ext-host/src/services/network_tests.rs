//! 联网 URL 解析与规范化的回归网。

use super::*;

#[test]
fn url_authority_parses_host_and_port() {
    assert_eq!(
        url_authority("https://api.example.com/v1/chat").unwrap(),
        ("api.example.com".into(), 443, 443)
    );
    assert_eq!(
        url_authority("http://api.example.com").unwrap(),
        ("api.example.com".into(), 80, 80)
    );
    assert_eq!(
        url_authority("https://api.example.com:8443/x?y=1#z").unwrap(),
        ("api.example.com".into(), 8443, 443)
    );
    // userinfo 不参与匹配。
    assert_eq!(
        url_authority("https://user:pw@api.example.com/x").unwrap(),
        ("api.example.com".into(), 443, 443)
    );
    // IPv6 保持方括号形式。
    assert_eq!(
        url_authority("http://[::1]:8080/x").unwrap(),
        ("[::1]".into(), 8080, 80)
    );
    assert_eq!(
        url_authority("HTTPS://API.Example.COM").unwrap(),
        ("api.example.com".into(), 443, 443)
    );
}

#[test]
fn url_authority_rejects_the_rest() {
    assert!(url_authority("ftp://h").is_err());
    assert!(url_authority("api.example.com").is_err());
    assert!(url_authority("https://").is_err());
    assert!(url_authority("https://h:0").is_err());
    assert!(url_authority("https://h:port").is_err());
    assert!(url_authority("http://[::1/x").is_err());
}

#[test]
fn normalize_caps_and_defaults() {
    let mut args = FetchArgs {
        url: "https://h".into(),
        method: None,
        headers: Vec::new(),
        body: None,
        limit: None,
        timeout_ms: None,
    };
    let normalized = normalize(args.clone());
    assert_eq!(normalized.limit, Some(FETCH_DEFAULT_LIMIT));
    assert_eq!(normalized.timeout_ms, Some(FETCH_DEFAULT_TIMEOUT_MS));

    args.limit = Some(0);
    args.timeout_ms = Some(0);
    assert_eq!(normalize(args.clone()).limit, Some(FETCH_DEFAULT_LIMIT));
    assert_eq!(
        normalize(args.clone()).timeout_ms,
        Some(FETCH_DEFAULT_TIMEOUT_MS)
    );

    args.limit = Some(u64::MAX);
    args.timeout_ms = Some(u32::MAX);
    assert_eq!(normalize(args.clone()).limit, Some(FETCH_MAX_LIMIT));
    assert_eq!(normalize(args).timeout_ms, Some(FETCH_MAX_TIMEOUT_MS));
}
