//! 联网 broker:扩展网络请求的唯一出口,独立进程,常驻服务不链接任何
//! 网络栈。
//!
//! 协议与原生扩展宿主一致:stdin 逐行 `{"method":"fetch","args":…}`,stdout 逐行
//! `{"result":…}` 或 `{"error":"…"}`,EOF 退出。域名白名单由宿主侧提供者
//! 按扩展强制(见 [`host_allowed`]);这里兜传输的底——响应体上限、时限、
//! 外部地址仅 HTTPS(明文只留给回环,规则在传输层)。未知方法与坏参数
//! 都按业务错误回 `error`,进程保持可用。

use std::io::{BufRead, Write};

pub use lueying_extproto::{
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

/// stdin 逐行服务请求,直到 EOF。`fetch` 是传输实现(可执行体接
/// WinHTTP,测试接假实现);这里只做协议、收敛与分发。读写失败即退出,
/// 宿主按应答缺失处置并重拉进程。
pub fn serve(
    mut fetch: impl FnMut(FetchArgs) -> Result<FetchResult, String>,
) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            return Ok(());
        };
        let reply = match handle(&line, &mut fetch) {
            Ok(result) => serde_json::json!({ "result": result }).to_string(),
            Err(error) => serde_json::json!({ "error": error }).to_string(),
        };
        writeln!(stdout, "{reply}").map_err(|error| format!("写应答失败: {error}"))?;
        stdout
            .flush()
            .map_err(|error| format!("写应答失败: {error}"))?;
    }
    Ok(())
}

fn handle(
    line: &str,
    fetch: &mut impl FnMut(FetchArgs) -> Result<FetchResult, String>,
) -> Result<FetchResult, String> {
    let envelope: serde_json::Value =
        serde_json::from_str(line).map_err(|error| format!("请求不是合法 JSON: {error}"))?;
    let method = envelope
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if method != "fetch" {
        return Err(format!("没有方法 {method:?}"));
    }
    let args: FetchArgs =
        serde_json::from_value(envelope.get("args").cloned().ok_or("请求缺 args")?)
            .map_err(|error| format!("联网参数不合法: {error}"))?;
    fetch(normalize(args))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
