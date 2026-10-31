//! 自动更新用到的纯逻辑:版本比较与 SHA-256 摘要提取。
//!
//! 放在协议 crate 里而不是 Windows 专属的 server 模块,测试就能在 Linux 上跑,
//! 不必为两个纯函数编译整棵带 Skia 的依赖树。

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
        if let Some(token) = line.split_whitespace().next()
            && token.len() == 64
            && token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Some(token.to_ascii_lowercase());
        }
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        (compact.len() == 64 && compact.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| compact.to_ascii_lowercase())
    })
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;
