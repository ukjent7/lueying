use super::*;

#[test]
fn whitelist_matches_default_and_explicit_ports() {
    assert!(allowed(&["api.example.com"], "api.example.com", 443, true));
    assert!(!allowed(&["api.example.com"], "api.example.com", 8443, true));
    assert!(allowed(
        &["localhost:11434"],
        "localhost",
        11434,
        false
    ));
    assert!(!allowed(&["localhost:11434"], "localhost", 8080, false));
}

#[test]
fn plain_http_only_allows_loopback() {
    assert!(http::split_url("http://localhost:8080/v1").is_ok());
    assert!(http::split_url("http://127.0.0.1:11434/api").is_ok());
    assert!(http::split_url("http://example.com/").is_err());
}

#[test]
fn rejects_unknown_method() {
    let line = r#"{"method":"post","args":{}}"#;
    assert!(handle_line(line).contains("未知方法"));
}

#[test]
fn rejects_domain_outside_whitelist() {
    let line = r#"{"method":"fetch","args":{"url":"https://example.com/","domains":["localhost"]}}"#;
    assert!(handle_line(line).contains("白名单"));
}
