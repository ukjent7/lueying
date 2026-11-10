//! 联网提供者:白名单按扩展强制,传输在本进程里经 WinHTTP 完成。
//!
//! 外部地址只许 HTTPS,明文只留给回环;响应体与时限有上限,结果必须是 UTF-8 文本。
//! 扩展卸载或停用时等待侧立刻取消,已在途的请求跑完后结果静默丢弃。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lueying_ext::host::{Done, Provider};
use lueying_ext::proto::{
    FETCH_DEFAULT_LIMIT, FETCH_DEFAULT_TIMEOUT_MS, FETCH_MAX_LIMIT, FETCH_MAX_TIMEOUT_MS,
    FetchArgs, FetchResult,
};

/// 收敛调用方给的体积与时限:缺省归默认,越界封顶,零视为缺省。
pub fn normalize(mut args: FetchArgs) -> FetchArgs {
    let limit = args.limit.take().filter(|limit| *limit > 0);
    args.limit = Some(limit.unwrap_or(FETCH_DEFAULT_LIMIT).min(FETCH_MAX_LIMIT));
    let timeout = args.timeout_ms.take().filter(|timeout| *timeout > 0);
    args.timeout_ms = Some(
        timeout
            .unwrap_or(FETCH_DEFAULT_TIMEOUT_MS)
            .min(FETCH_MAX_TIMEOUT_MS),
    );
    args
}

/// URL 的主机段与端口;`https` 缺省 443,`http` 缺省 80,其余 scheme
/// 拒绝。主机统一小写;IPv6 保持方括号形式,好与白名单条目对表。
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

/// 白名单是否放行该 URL。条目 `host` 只放行 scheme 默认端口;带端口的
/// 条目精确匹配实际端口。比较大小写不敏感。
pub fn host_allowed(allow: &[String], url: &str) -> Result<bool, String> {
    let (host, port, default) = url_authority(url)?;
    Ok(allow.iter().any(|entry| {
        entry.eq_ignore_ascii_case(&host) && port == default
            || entry.eq_ignore_ascii_case(&format!("{host}:{port}"))
    }))
}

/// 联网提供者:白名单按扩展强制(域名清单来自扩展清单并经用户同意)。
pub struct NetworkProvider {
    /// 扩展 id → 白名单;每次扩展重载时整体重建,没登记的扩展一律拒绝。
    allow: Mutex<HashMap<String, Arc<[String]>>>,
    /// 全局免密直通:跳过逐域名白名单,只保留 URL 合法性检查。
    open_all: AtomicBool,
}

impl NetworkProvider {
    /// 空提供者:没有任何扩展有白名单,也没有免密直通。
    pub fn new() -> Self {
        Self {
            allow: Mutex::new(HashMap::new()),
            open_all: AtomicBool::new(false),
        }
    }

    /// 灌入本次扩展重载的白名单;整体替换。
    pub fn set_allow(&self, grants: Vec<(String, Vec<String>)>) {
        let map = grants
            .into_iter()
            .map(|(id, allow)| (id, Arc::from(allow.into_boxed_slice())))
            .collect();
        *self.allow.lock().expect("联网白名单锁") = map;
    }

    /// 设置全局免密直通;配置变化后由扩展宿主重建时调用。
    pub fn set_open_all(&self, open: bool) {
        self.open_all.store(open, Ordering::Relaxed);
    }
}

impl Provider for NetworkProvider {
    fn call(&self, caller: &str, _method: &str, args: &str, done: Done) -> Result<(), String> {
        let allow = self
            .allow
            .lock()
            .expect("联网白名单锁")
            .get(caller)
            .map(|allow| allow.to_vec())
            .unwrap_or_default();
        let args: FetchArgs =
            serde_json::from_str(args).map_err(|_| "联网参数不是合法 JSON".to_string())?;
        if self.open_all.load(Ordering::Relaxed) {
            if let Err(error) = url_authority(&args.url) {
                done.complete(Err(error));
                return Ok(());
            }
        } else {
            match host_allowed(&allow, &args.url) {
                Ok(true) => {}
                Ok(false) => {
                    let host = url_authority(&args.url)
                        .map(|(host, _, _)| host)
                        .unwrap_or_else(|_| args.url.clone());
                    done.complete(Err(format!("域名 {host} 不在扩展声明的白名单里")));
                    return Ok(());
                }
                Err(error) => {
                    done.complete(Err(error));
                    return Ok(());
                }
            }
        }
        std::thread::Builder::new()
            .name("ext-network".into())
            .spawn(move || {
                let reply = fetch(normalize(args)).and_then(|reply| {
                    serde_json::to_string(&reply).map_err(|error| error.to_string())
                });
                done.complete(reply);
            })
            .map(|_| ())
            .map_err(|error| format!("联网线程启动失败: {error}"))
    }
}

/// 受限传输:GET/POST 等经系统 WinHTTP,响应体按上限读回。
#[cfg(windows)]
fn fetch(args: FetchArgs) -> Result<FetchResult, String> {
    let request = lueying_net::http::Request {
        method: args.method.unwrap_or_else(|| "GET".to_string()),
        url: args.url,
        headers: args.headers,
        body: args.body.map(String::into_bytes),
        limit: args.limit.unwrap_or(FETCH_DEFAULT_LIMIT),
        timeout_ms: args.timeout_ms.unwrap_or(FETCH_DEFAULT_TIMEOUT_MS),
        credentials: None,
    };
    let response = lueying_net::http::send(&request)?;
    // 文本契约:二进制响应报错,不静默替换字节。
    let body = String::from_utf8(response.body)
        .map_err(|_| format!("响应不是 UTF-8 文本(状态 {})", response.status))?;
    Ok(FetchResult {
        status: response.status,
        body,
    })
}

/// 非 Windows 平台没有受限传输;联网调用明确失败。
#[cfg(not(windows))]
fn fetch(_args: FetchArgs) -> Result<FetchResult, String> {
    Err("联网只在 Windows 上可用".into())
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
