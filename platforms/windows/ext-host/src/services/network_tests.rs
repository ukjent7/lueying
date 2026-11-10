//! 联网参数规范化的回归网(URL 解析的回归网在 `lueying_ext::manifest::network`)。

use super::*;

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
