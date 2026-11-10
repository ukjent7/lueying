//! 出站代理的选择:环境变量优先,没有就读 Windows 系统代理(Clash、v2rayN 等设置的是它)。
//!
//! 这里只判断「这个地址走哪个代理、还是直连」,产出代理 URL 交给调用方的 HTTP 客户端。
//! 回环地址永远直连。不支持 PAC 脚本与 WPAD 自动发现。

/// 系统代理设置:注册表 `ProxyServer` 与 `ProxyOverride` 的原样内容。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemProxy {
    /// `host:port`,或 `http=...;https=...;socks=...` 分协议的形式。
    pub server: String,
    /// 绕过列表,`;` 分隔,支持 `<local>` 与 `*` 通配。
    pub bypass: String,
}

/// 环境变量里的代理:代理地址与 `NO_PROXY` 绕过列表。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvProxy {
    /// 代理地址,可能没有协议前缀。
    pub proxy: String,
    /// `NO_PROXY`,逗号分隔。
    pub no_proxy: String,
}

/// 完整地址该走的代理 URL(如 `http://127.0.0.1:7890`);`None` 表示直连。
pub fn proxy_for(url: &str) -> Option<String> {
    let (scheme, host) = split_target(url)?;
    choose(&scheme, &host, env_proxy(&scheme), system_proxy())
}

/// 纯决策:给定目标(主机不带方括号,协议小写)与两个来源,返回代理 URL。环境变量一旦设置就以它为准。
pub fn choose(
    scheme: &str,
    host: &str,
    env: Option<EnvProxy>,
    system: Option<SystemProxy>,
) -> Option<String> {
    if is_loopback(host) {
        return None;
    }
    if let Some(env) = env {
        return (!bypassed(&env.no_proxy, host)).then(|| with_scheme(&env.proxy, "http"));
    }
    let system = system?;
    if bypassed(&system.bypass, host) {
        return None;
    }
    server_for(&system.server, scheme)
}

/// 系统代理串里对应协议的代理。`host:port` 对所有协议生效;分协议形式按协议取,
/// 没有对应协议时退到 `socks`。
pub fn server_for(server: &str, scheme: &str) -> Option<String> {
    let server = server.trim();
    if server.is_empty() {
        return None;
    }
    if !server.contains('=') {
        return Some(with_scheme(server, "http"));
    }
    let mut matched = None;
    let mut socks = None;
    for entry in server.split(';') {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if key.starts_with("socks") {
            socks = Some(with_scheme(value, "socks5"));
        } else if key == scheme {
            matched = Some(with_scheme(value, "http"));
        }
    }
    matched.or(socks)
}

/// 地址是否落在绕过列表里。`<local>` 匹配不含点的主机名;其余按 `*` 通配、不分大小写。
/// 列表可用 `;` 或 `,` 分隔。
pub fn bypassed(list: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    list.split([';', ','])
        .map(|pattern| pattern.trim().to_ascii_lowercase())
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| {
            if pattern == "<local>" {
                !host.contains('.')
            } else {
                glob(&pattern, &host)
            }
        })
}

/// 只认 `*` 的通配匹配;`text` 与 `pattern` 都已小写。
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, middle @ .., last] = parts.as_slice() else {
        return pattern == text;
    };
    let Some(mut rest) = text.strip_prefix(*first) else {
        return false;
    };
    for part in middle {
        match rest.find(*part) {
            Some(index) => rest = &rest[index + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(*last)
}

/// 没有协议前缀的代理地址补上 `default` 协议;已有协议的原样保留。
fn with_scheme(value: &str, default: &str) -> String {
    if value.contains("://") {
        value.to_string()
    } else {
        format!("{default}://{value}")
    }
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// 从地址取协议与主机(小写、去掉用户信息与端口,IPv6 去掉方括号)。
fn split_target(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => authority.split(':').next()?,
    };
    Some((scheme.to_ascii_lowercase(), host.to_ascii_lowercase()))
}

/// 环境变量代理:协议专用变量优先,其次 `ALL_PROXY`。名字同时认大写与小写。
pub fn env_proxy(scheme: &str) -> Option<EnvProxy> {
    let names: &[&str] = if scheme == "https" {
        &["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
    } else {
        &["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
    };
    let proxy = env_first(names)?;
    let no_proxy = env_first(&["NO_PROXY", "no_proxy"]).unwrap_or_default();
    Some(EnvProxy { proxy, no_proxy })
}

fn env_first(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

/// 当前用户的系统代理:只有 `ProxyEnable` 为 1 时才生效。非 Windows 没有这一层。
#[cfg(windows)]
pub fn system_proxy() -> Option<SystemProxy> {
    if reg_dword("ProxyEnable") != Some(1) {
        return None;
    }
    Some(SystemProxy {
        server: reg_string("ProxyServer")?,
        bypass: reg_string("ProxyOverride").unwrap_or_default(),
    })
}

/// 非 Windows 没有系统代理设置。
#[cfg(not(windows))]
pub fn system_proxy() -> Option<SystemProxy> {
    None
}

#[cfg(windows)]
const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn reg_dword(name: &str) -> Option<u32> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::PCWSTR;

    let key = wide(INTERNET_SETTINGS);
    let name = wide(name);
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::addr_of_mut!(data).cast()),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(data)
}

#[cfg(windows)]
fn reg_string(name: &str) -> Option<String> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::PCWSTR;

    let key = wide(INTERNET_SETTINGS);
    let name = wide(name);
    // 两段调用:先问长度(字节),再按长度读。
    let mut bytes = 0u32;
    let probe = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
    };
    if probe.is_err() {
        return None;
    }
    let mut buffer = vec![0u16; bytes as usize / 2 + 1];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(proxy: &str, no_proxy: &str) -> Option<EnvProxy> {
        Some(EnvProxy {
            proxy: proxy.to_string(),
            no_proxy: no_proxy.to_string(),
        })
    }

    fn system(server: &str, bypass: &str) -> Option<SystemProxy> {
        Some(SystemProxy {
            server: server.to_string(),
            bypass: bypass.to_string(),
        })
    }

    #[test]
    fn plain_host_port_applies_to_every_scheme() {
        assert_eq!(
            server_for("127.0.0.1:7890", "https").as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(
            server_for("127.0.0.1:7890", "http").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn per_scheme_form_picks_matching_protocol() {
        let server = "http=127.0.0.1:7890;https=127.0.0.1:7891";
        assert_eq!(
            server_for(server, "https").as_deref(),
            Some("http://127.0.0.1:7891")
        );
        assert_eq!(
            server_for(server, "http").as_deref(),
            Some("http://127.0.0.1:7890")
        );
    }

    #[test]
    fn per_scheme_form_falls_back_to_socks() {
        let server = "http=127.0.0.1:7890;socks=127.0.0.1:7891";
        assert_eq!(
            server_for(server, "https").as_deref(),
            Some("socks5://127.0.0.1:7891")
        );
        assert_eq!(
            server_for("socks=127.0.0.1:7891", "http").as_deref(),
            Some("socks5://127.0.0.1:7891")
        );
    }

    #[test]
    fn explicit_scheme_and_empty_values() {
        assert_eq!(
            server_for("http://proxy.example:8080", "https").as_deref(),
            Some("http://proxy.example:8080")
        );
        assert_eq!(server_for("   ", "https"), None);
        assert_eq!(server_for("https=;http=", "https"), None);
    }

    #[test]
    fn bypass_handles_local_wildcards_and_exact_names() {
        assert!(bypassed("<local>", "nas"));
        assert!(!bypassed("<local>", "github.com"));
        assert!(bypassed("*.corp.example", "a.corp.example"));
        assert!(bypassed("192.168.*", "192.168.1.5"));
        assert!(!bypassed("192.168.*", "10.0.0.1"));
        assert!(bypassed("localhost", "LOCALHOST"));
        assert!(!bypassed("*.local", "github.com"));
        assert!(bypassed("<local>;*.corp.example", "wiki.corp.example"));
        assert!(bypassed("a.example, b.example", "b.example"));
    }

    #[test]
    fn glob_only_matches_whole_patterns() {
        assert!(glob("*", "anything"));
        assert!(glob("a*", "abc"));
        assert!(!glob("a*a", "a"));
        assert!(glob("a*a", "aba"));
        assert!(!glob("exact", "exact.more"));
    }

    #[test]
    fn env_proxy_wins_over_system_proxy() {
        assert_eq!(
            choose(
                "https",
                "github.com",
                env("http://env:1", ""),
                system("127.0.0.1:7890", "")
            )
            .as_deref(),
            Some("http://env:1")
        );
    }

    #[test]
    fn no_proxy_sends_matching_host_direct() {
        assert_eq!(
            choose(
                "https",
                "api.corp.example",
                env("http://env:1", "*.corp.example"),
                system("127.0.0.1:7890", "")
            ),
            None
        );
    }

    #[test]
    fn env_proxy_without_scheme_gets_http() {
        assert_eq!(
            choose("https", "github.com", env("env:1", ""), None).as_deref(),
            Some("http://env:1")
        );
    }

    #[test]
    fn system_proxy_respects_bypass_list() {
        let system = system("127.0.0.1:7890", "<local>;*.intranet");
        assert_eq!(
            choose("https", "github.com", None, system.clone()).as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(choose("https", "wiki.intranet", None, system.clone()), None);
        assert_eq!(choose("https", "nas", None, system), None);
    }

    #[test]
    fn loopback_is_always_direct() {
        assert_eq!(
            choose(
                "http",
                "localhost",
                env("http://env:1", ""),
                system("127.0.0.1:7890", "")
            ),
            None
        );
        assert_eq!(
            choose("http", "::1", None, system("127.0.0.1:7890", "")),
            None
        );
    }

    #[test]
    fn no_sources_means_direct() {
        assert_eq!(choose("https", "github.com", None, None), None);
    }

    #[test]
    fn split_target_reads_host_port_and_userinfo() {
        assert_eq!(
            split_target("https://API.github.com/repos/x?y#z"),
            Some(("https".to_string(), "api.github.com".to_string()))
        );
        assert_eq!(
            split_target("http://user:pw@127.0.0.1:8080/a"),
            Some(("http".to_string(), "127.0.0.1".to_string()))
        );
        assert_eq!(
            split_target("https://[::1]:443/"),
            Some(("https".to_string(), "::1".to_string()))
        );
        assert_eq!(split_target("no-scheme"), None);
    }
}
