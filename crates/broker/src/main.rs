//! 联网 broker 可执行体:stdio JSON 行协议接 [`lueying_net`] 的受限
//! HTTP。宿主经旁路层按需拉起、空闲收走;进程本身不碰任何输入法状态。

#[cfg(windows)]
fn main() {
    if let Err(error) = lueying_broker::serve(fetch) {
        eprintln!("broker: {error}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn fetch(args: lueying_broker::FetchArgs) -> Result<lueying_broker::FetchResult, String> {
    let request = lueying_net::http::Request {
        method: args.method.unwrap_or_else(|| "GET".to_string()),
        url: args.url,
        headers: args.headers,
        body: args.body.map(String::into_bytes),
        limit: args.limit.unwrap_or(lueying_broker::FETCH_DEFAULT_LIMIT),
        timeout_ms: args
            .timeout_ms
            .unwrap_or(lueying_broker::FETCH_DEFAULT_TIMEOUT_MS),
        credentials: None,
    };
    let response = lueying_net::http::send(&request)?;
    // 文本契约:二进制响应报错,不静默替换字节。
    let body = String::from_utf8(response.body)
        .map_err(|_| format!("响应不是 UTF-8 文本(状态 {})", response.status))?;
    Ok(lueying_broker::FetchResult {
        status: response.status,
        body,
    })
}

#[cfg(not(windows))]
fn main() {}
