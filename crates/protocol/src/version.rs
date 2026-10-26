//! 自动更新用到的纯逻辑:版本比较与 SHA-256 摘要提取。
//!
//! 放在协议 crate 里而不是 Windows 专属的 server 模块,测试就能在 Linux 上跑,
//! 不必为两个纯函数编译整棵 GPUI 依赖树。

use std::cmp::Ordering;

/// 候选版本是否比当前版本新。预发布版本低于同号正式版。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    compare_versions(candidate, current) == Ordering::Greater
}

fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a_core, a_pre) = split_version(a);
    let (b_core, b_pre) = split_version(b);
    let core = compare_core(a_core, b_core);
    if core != Ordering::Equal {
        return core;
    }
    match (a_pre, b_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => compare_pre(a, b),
    }
}

fn split_version(version: &str) -> (&str, Option<&str>) {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    }
}

fn compare_core(a: &str, b: &str) -> Ordering {
    let mut a = a.split('.');
    let mut b = b.split('.');
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (x, y) => {
                let x = x.and_then(|part| part.parse::<u64>().ok()).unwrap_or(0);
                let y = y.and_then(|part| part.parse::<u64>().ok()).unwrap_or(0);
                match x.cmp(&y) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
        }
    }
}

fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut a = a.split('.');
    let mut b = b.split('.');
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// 从 `sha256sum` 或 `certutil -hashfile` 的输出里取摘要。
pub fn parse_sha256(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        // sha256sum 格式是 `<hash>  <file>`;certutil 有的版本把摘要按 2 位一组
        // 用空格分开,行上只有摘要。两种都接受。
        if let Some(token) = line.split_whitespace().next() {
            if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Some(token.to_ascii_lowercase());
            }
        }
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        (compact.len() == 64 && compact.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| compact.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
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
}
