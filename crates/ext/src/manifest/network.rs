//! 联网白名单的匹配:URL 的主机与端口对表条目。
//!
//! 条目是 `host` 或 `host:port`(IPv6 写作 `[addr]:port`)。不带端口的条目放行
//! 该主机在 URL scheme 下的默认端口(https 443、http 80);带端口的条目只放行
//! 那个端口。主机放行它本身及其子域名,`evil-example.com` 不是 `example.com`
//! 的子域名。IP 字面量只做精确匹配。

use std::net::Ipv4Addr;

use crate::manifest::NetworkGrant;

impl NetworkGrant {
    /// URL 能否访问:主机与端口都要落在某条白名单里。URL 不合法一律拒绝。
    pub fn allows(&self, url: &str) -> bool {
        let Ok((host, port, default)) = url_authority(url) else {
            return false;
        };
        self.allow
            .iter()
            .any(|entry| entry_allows(entry, &host, port, default))
    }
}

/// 一条条目是否放行 `host:port`;`default` 是 URL scheme 的默认端口。
fn entry_allows(entry: &str, host: &str, port: u16, default: u16) -> bool {
    let (name, entry_port) = split_entry(entry);
    if entry_port.unwrap_or(default) != port {
        return false;
    }
    if host == name {
        return true;
    }
    !is_ip(&name) && !is_ip(host) && host.ends_with(&format!(".{name}"))
}

/// 条目拆成小写主机与可选端口;条目已由清单校验过格式。
fn split_entry(entry: &str) -> (String, Option<u16>) {
    let (host, port) = match entry.strip_prefix('[') {
        Some(body) => match body.split_once(']') {
            Some((addr, rest)) => (format!("[{addr}]"), rest.strip_prefix(':')),
            None => (entry.to_string(), None),
        },
        None => match entry.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), Some(port)),
            None => (entry.to_string(), None),
        },
    };
    (
        host.to_ascii_lowercase(),
        port.and_then(|port| port.parse().ok()),
    )
}

fn is_ip(host: &str) -> bool {
    host.starts_with('[') || host.parse::<Ipv4Addr>().is_ok()
}

/// URL 的主机段与端口;`https` 缺省 443,`http` 缺省 80,其余 scheme 拒绝。
/// 主机统一小写;IPv6 保持方括号形式,好与白名单条目对表。userinfo 不参与。
pub fn url_authority(url: &str) -> Result<(String, u16, u16), String> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err("URL 缺 ://".into());
    };
    let default = match scheme.to_ascii_lowercase().as_str() {
        "https" => 443,
        "http" => 80,
        other => return Err(format!("不支持的 URL scheme {other:?}")),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = match authority.rsplit_once('@') {
        Some((_, host)) => host,
        None => authority,
    };
    let (host, port) = if let Some(body) = authority.strip_prefix('[') {
        let Some(end) = body.find(']') else {
            return Err("URL 的 IPv6 缺右括号".into());
        };
        let (v6, tail) = (&body[..end], &body[end + 1..]);
        match tail.strip_prefix(':') {
            Some(port) => (format!("[{v6}]"), port),
            None if tail.is_empty() => (format!("[{v6}]"), ""),
            None => return Err("URL 的 IPv6 后应为 :端口".into()),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), port),
            None => (authority.to_string(), ""),
        }
    };
    if host.is_empty() {
        return Err("URL 缺主机名".into());
    }
    let port = match port.parse::<u16>() {
        Ok(number) if number > 0 => number,
        Ok(_) => return Err("URL 端口不能是 0".into()),
        Err(_) if port.is_empty() => default,
        Err(_) => return Err("URL 端口不合法".into()),
    };
    Ok((host.to_ascii_lowercase(), port, default))
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
