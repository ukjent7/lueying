//! 联网提供者:传输在本进程里经 ureq 完成,代理按系统设置(见 `lueying_ext::proxy`)。
//! 域名白名单在调用方(Wasm 宿主的 `network.fetch`)逐请求核对,这里只做 URL 合法性检查。
//! 重定向不跟随:跳转到名单外的地址会变成一次新的、未经核对的请求,所以 3xx 原样交给扩展。
//!
//! 外部地址只许 HTTPS,明文只留给回环;响应体与时限有上限,结果必须是 UTF-8 文本。
//! 扩展卸载或停用时等待侧立刻取消,已在途的请求跑完后结果静默丢弃。

use std::time::Duration;

use lueying_ext::host::{Done, Provider};
use lueying_ext::manifest::network::url_authority;
use lueying_ext::proto::{
    FETCH_DEFAULT_LIMIT, FETCH_DEFAULT_TIMEOUT_MS, FETCH_MAX_LIMIT, FETCH_MAX_TIMEOUT_MS,
    FetchArgs, FetchResult,
};
use ureq::tls::{RootCerts, TlsConfig};

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

/// 联网提供者。没有状态;白名单在调用方核对。
#[derive(Default)]
pub struct NetworkProvider;

impl NetworkProvider {
    /// 联网提供者没有状态。
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

/// 受限传输:HTTPS 直连(明文 HTTP 只许回环),非 2xx 也照常返回,响应体按上限读回。
fn fetch(args: FetchArgs) -> Result<FetchResult, String> {
    let scheme = args
        .url
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .unwrap_or_default();
    let (host, _, _) = url_authority(&args.url)?;
    let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]");
    if scheme == "http" && !loopback {
        return Err("明文 HTTP 只允许本机回环地址".into());
    }
    let proxy = match lueying_ext::proxy::proxy_for(&args.url) {
        Some(proxy) => Some(
            ureq::Proxy::new(&proxy).map_err(|error| format!("代理设置不可用 {proxy}: {error}"))?,
        ),
        None => None,
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(u64::from(
            args.timeout_ms.unwrap_or(FETCH_DEFAULT_TIMEOUT_MS),
        ))))
        .http_status_as_error(false)
        // 不跟随重定向:跳转目标没经过白名单核对。
        .max_redirects(0)
        // 外部地址也不许降到明文;回环地址本就是明文。
        .https_only(scheme == "https")
        .proxy(proxy)
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into();
    let method = args.method.unwrap_or_else(|| "GET".to_string());
    let mut request = ureq::http::Request::builder()
        .method(method.as_str())
        .uri(args.url.as_str());
    for (name, value) in &args.headers {
        request = request.header(name, value);
    }
    let body = args.body.map(String::into_bytes).unwrap_or_default();
    let request = request
        .body(body)
        .map_err(|error| format!("请求不合法: {error}"))?;
    let mut response = agent
        .run(request)
        .map_err(|error| format!("请求失败: {error}"))?;
    let status = response.status().as_u16();
    let limit = args.limit.unwrap_or(FETCH_DEFAULT_LIMIT);
    let bytes = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|error| format!("读响应: {error}"))?;
    // 文本契约:二进制响应报错,不静默替换字节。
    let body =
        String::from_utf8(bytes).map_err(|_| format!("响应不是 UTF-8 文本(状态 {status})"))?;
    Ok(FetchResult { status, body })
}

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
