//! 联网提供者:传输在本进程里经 WinHTTP 完成。扩展的域名清单在安装时亮给用户同意;
//! 同意之后宿主不再逐域名拦截,这里只做 URL 合法性检查。
//!
//! 外部地址只许 HTTPS,明文只留给回环;响应体与时限有上限,结果必须是 UTF-8 文本。
//! 扩展卸载或停用时等待侧立刻取消,已在途的请求跑完后结果静默丢弃。

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

/// 联网提供者。同意之后不再逐域名拦截。
#[derive(Default)]
pub struct NetworkProvider;

impl NetworkProvider {
    /// 联网提供者没有状态:扩展的域名清单只在安装时同意一次。
    pub fn new() -> Self {
        Self
    }
}

impl Provider for NetworkProvider {
    fn call(&self, _caller: &str, _method: &str, args: &str, done: Done) -> Result<(), String> {
        let args: FetchArgs =
            serde_json::from_str(args).map_err(|_| "联网参数不是合法 JSON".to_string())?;
        if let Err(error) = url_authority(&args.url) {
            done.complete(Err(error));
            return Ok(());
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
