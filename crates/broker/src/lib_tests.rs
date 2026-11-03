//! broker 协议与白名单匹配的回归网。

use super::*;

fn allow(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| entry.to_string()).collect()
}

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
fn host_matches_default_and_explicit_ports() {
    let entries = allow(&["api.example.com", "dev.local:8080", "[::1]:8080"]);
    // 裸条目只放行 scheme 默认端口。
    assert!(host_allowed(&entries, "https://api.example.com/v1").unwrap());
    assert!(host_allowed(&entries, "https://API.example.COM:443/v1").unwrap());
    assert!(!host_allowed(&entries, "https://api.example.com:8443/v1").unwrap());
    // 带端口条目精确匹配端口。
    assert!(host_allowed(&entries, "http://dev.local:8080/x").unwrap());
    assert!(!host_allowed(&entries, "http://dev.local/x").unwrap());
    assert!(!host_allowed(&entries, "http://other.local:8080/x").unwrap());
    assert!(host_allowed(&entries, "http://[::1]:8080/x").unwrap());
    // 白名单外的 host 一律不放行。
    assert!(!host_allowed(&entries, "https://evil.example.com").unwrap());
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

#[test]
fn handle_serves_fetch_and_rejects_the_rest() {
    let seen = std::sync::Mutex::new(Vec::new());
    let mut fetch = |args: FetchArgs| {
        seen.lock().unwrap().push(args.clone());
        Ok(FetchResult {
            status: 200,
            body: "你好".into(),
        })
    };

    let result = handle(
        r#"{"method":"fetch","args":{"url":"https://api.example.com","limit":100}}"#,
        &mut fetch,
    )
    .unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.body, "你好");
    let args = &seen.lock().unwrap()[0];
    assert_eq!(args.url, "https://api.example.com");
    assert_eq!(args.limit, Some(100));
    assert_eq!(args.timeout_ms, Some(FETCH_DEFAULT_TIMEOUT_MS));

    assert!(
        handle(r#"{"method":"post","args":{}}"#, &mut fetch)
            .unwrap_err()
            .contains("没有方法")
    );
    assert!(
        handle("不是 JSON", &mut fetch)
            .unwrap_err()
            .contains("不是合法 JSON")
    );
    assert!(
        handle(r#"{"method":"fetch"}"#, &mut fetch)
            .unwrap_err()
            .contains("缺 args")
    );
    assert!(
        handle(r#"{"method":"fetch","args":{}}"#, &mut fetch)
            .unwrap_err()
            .contains("联网参数不合法")
    );
}
