//! 扩展网络代理:一行请求、一行应答;域名白名单在这里与宿主双重校验。
//!
//! 宿主把清单里的 `network.domains` 随请求带进来。代理只接受 HTTPS,以及
//! `localhost`/`127.0.0.1`/`[::1]` 的明文 HTTP;域名或端口不在名单里直接
//! 拒绝。业务扩展永远拿不到 WinHTTP 句柄,常驻 Server 也不链接 WinHTTP。

use serde_json::{Value, json};

use crate::http::{self, Request};

/// 单次响应体上限;扩展请求是结构化数据,不搬运大文件。
pub const MAX_BODY: u64 = 512 * 1024;

/// 处理一行请求;返回一行 JSON 应答。协议与旁路进程一致。
pub fn handle_line(line: &str) -> String {
    match handle(line) {
        Ok(value) => json!({ "result": value }).to_string(),
        Err(error) => json!({ "error": error }).to_string(),
    }
}

fn handle(line: &str) -> Result<Value, String> {
    let request: Value =
        serde_json::from_str(line).map_err(|error| format!("请求不是 JSON: {error}"))?;
    if request["method"].as_str() != Some("fetch") {
        return Err(format!("未知方法 {:?}", request["method"]));
    }
    let args = &request["args"];
    let url = args["url"]
        .as_str()
        .ok_or_else(|| "缺 url".to_string())?;
    let domains = args["domains"]
        .as_array()
        .ok_or_else(|| "缺 domains".to_string())?;
    let domains: Vec<&str> = domains.iter().filter_map(Value::as_str).collect();
    let (secure, host, port, _) = http::split_url(url)?;
    if !allowed(&domains, &host, port, secure) {
        return Err(format!("域名不在清单白名单: {host}:{port}"));
    }
    let method = args["method"].as_str().unwrap_or("GET").to_ascii_uppercase();
    let headers = args["headers"]
        .as_object()
        .map(|table| {
            table
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let body = args["body"].as_str().map(|text| text.as_bytes().to_vec());
    let response = http::send(&Request {
        method,
        url: url.to_string(),
        headers,
        body,
        limit: MAX_BODY,
        timeout_ms: 20_000,
    })?;
    Ok(json!({
        "status": response.status,
        "body": String::from_utf8_lossy(&response.body),
    }))
}

/// 名单项可以写 `host` 或 `host:port`;不写端口时只匹配该协议的默认端口。
fn allowed(domains: &[&str], host: &str, port: u16, secure: bool) -> bool {
    let default_port = if secure { 443 } else { 80 };
    domains.iter().any(|domain| {
        let domain = domain.trim().to_ascii_lowercase();
        match domain.rsplit_once(':') {
            Some((name, port_text)) if !port_text.is_empty() => {
                name == host && port_text.parse::<u16>().ok() == Some(port)
            }
            _ => domain == host && port == default_port,
        }
    })
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
