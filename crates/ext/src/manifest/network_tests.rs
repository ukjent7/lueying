//! 联网白名单的匹配与 URL 解析的回归网。

use super::*;

fn grant(allow: &[&str]) -> NetworkGrant {
    NetworkGrant {
        allow: allow.iter().map(|entry| entry.to_string()).collect(),
    }
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
fn host_entry_allows_itself_and_subdomains() {
    let g = grant(&["example.com"]);
    assert!(g.allows("https://example.com/x"));
    assert!(g.allows("https://api.example.com/x"));
    assert!(g.allows("https://a.b.example.com/x"));
    assert!(g.allows("HTTPS://API.EXAMPLE.COM/x"));
}

#[test]
fn subdomain_match_respects_label_boundary() {
    let g = grant(&["example.com"]);
    assert!(!g.allows("https://evilexample.com/x"));
    assert!(!g.allows("https://example.com.evil.net/x"));
    assert!(!g.allows("https://example.org/x"));
    // 子域名条目不放行父域。
    let sub = grant(&["api.example.com"]);
    assert!(sub.allows("https://v2.api.example.com/x"));
    assert!(!sub.allows("https://example.com/x"));
    assert!(!sub.allows("https://other.example.com/x"));
}

#[test]
fn userinfo_cannot_fake_the_host() {
    let g = grant(&["example.com"]);
    assert!(!g.allows("https://example.com@evil.net/x"));
    assert!(g.allows("https://user@api.example.com/x"));
}

#[test]
fn portless_entry_allows_only_scheme_default_port() {
    let g = grant(&["example.com"]);
    assert!(g.allows("https://example.com:443/x"));
    assert!(!g.allows("https://example.com:8443/x"));
    assert!(!g.allows("https://api.example.com:8443/x"));
    assert!(g.allows("http://example.com/x"));
    assert!(!g.allows("http://example.com:8080/x"));
}

#[test]
fn port_entry_allows_only_that_port() {
    let g = grant(&["example.com:8443"]);
    assert!(g.allows("https://example.com:8443/x"));
    assert!(g.allows("https://api.example.com:8443/x"));
    assert!(!g.allows("https://example.com/x"));
    assert!(!g.allows("https://example.com:9443/x"));
}

#[test]
fn ipv6_entry_matches_bracketed_host_and_port() {
    let g = grant(&["[::1]:8080"]);
    assert!(g.allows("http://[::1]:8080/x"));
    assert!(!g.allows("http://[::1]/x"));
    assert!(!g.allows("http://[::2]:8080/x"));
}

#[test]
fn ip_literals_are_exact_only() {
    let g = grant(&["0.0.1"]);
    assert!(!g.allows("https://10.0.0.1/x"));
    let ip = grant(&["1.2.3.4"]);
    assert!(ip.allows("https://1.2.3.4/x"));
    assert!(!ip.allows("https://9.1.2.3.4/x"));
}

#[test]
fn bad_urls_and_empty_grants_are_refused() {
    let g = grant(&["example.com"]);
    assert!(!g.allows("ftp://example.com/x"));
    assert!(!g.allows("example.com"));
    assert!(!grant(&[]).allows("https://example.com/x"));
}
