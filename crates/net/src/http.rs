//! 受限 HTTP 客户端:HTTPS 或本机回环明文 HTTP。
//!
//! 更新器与扩展网络代理共用这一层。外部地址只允许 HTTPS;`http://` 仅供
//! `localhost`/`127.0.0.1`/`[::1]`,给本地模型与开发服务用。重定向禁止
//! HTTPS 降到 HTTP,响应体按调用方给的字节上限截断前拒绝。

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::GetLastError;
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// 一次请求。`headers` 是名称/值对;值里不能有换行。
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub limit: u64,
    pub timeout_ms: u32,
}

/// 一次响应;非 2xx 也照常返回,由调用方判断。
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// 发送请求。响应体超过 `limit` 时整次调用失败,不返回半截数据。
pub fn send(request: &Request) -> Result<Response, String> {
    let (secure, host, port, path) = split_url(&request.url)?;
    let response = ResponseHandle::open(request, secure, &host, port, &path)?;
    let status = response.status()?;
    let body = response.bytes(request.limit)?;
    Ok(Response { status, body })
}

/// 流式响应:边收边处理,不把整个响应体读进内存。给更新包这类大文件用。
pub struct Stream {
    handle: ResponseHandle,
    status: u16,
}

impl Stream {
    /// 发出 GET 并等响应头;响应体上限在 [`Stream::read_chunks`] 里给。
    pub fn open(url: &str, timeout_ms: u32) -> Result<Self, String> {
        let request = Request {
            method: "GET".to_string(),
            url: url.to_string(),
            headers: Vec::new(),
            body: None,
            limit: 0,
            timeout_ms,
        };
        let (secure, host, port, path) = split_url(url)?;
        let handle = ResponseHandle::open(&request, secure, &host, port, &path)?;
        let status = handle.status()?;
        Ok(Self { handle, status })
    }

    /// 非 2xx 也照常打开,由调用方判断。
    pub fn status(&self) -> u16 {
        self.status
    }

    /// 逐块交给 `on_chunk`,返回总字节数;超过 `limit` 立即失败。
    pub fn read_chunks<F>(&self, limit: u64, on_chunk: F) -> Result<u64, String>
    where
        F: FnMut(&[u8]) -> Result<(), String>,
    {
        self.handle.read_chunks(limit, on_chunk)
    }
}

/// 解析 URL 并做协议约束;返回 (是否 HTTPS, 主机, 端口, 路径)。
pub fn split_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("地址必须是 http:// 或 https://: {url}"));
    };
    let (authority, rest) = match rest.find(['/', '?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("地址缺少主机名: {url}"));
    }
    let (host, port) = split_authority(authority)?;
    if !secure && !is_loopback(&host) {
        return Err(format!("明文 HTTP 只允许本机回环地址: {host}"));
    }
    let path = if rest.starts_with('/') {
        rest.to_string()
    } else {
        format!("/{rest}")
    };
    Ok((
        secure,
        host,
        port.unwrap_or(if secure { 443 } else { 80 }),
        path,
    ))
}

fn split_authority(authority: &str) -> Result<(String, Option<u16>), String> {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|ch| ch.is_ascii_digit()) && !port.is_empty() => {
            let port: u16 = port
                .parse()
                .map_err(|_| format!("端口不合法: {authority}"))?;
            if port == 0 {
                return Err(format!("端口不合法: {authority}"));
            }
            (host, Some(port))
        }
        _ => (authority, None),
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if host.is_empty()
        || !host
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':'))
    {
        return Err(format!("主机名不合法: {authority}"));
    }
    Ok((host, port))
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn last_error(what: &str) -> String {
    let code = unsafe { GetLastError() };
    format!("{what}(Win32 错误 {})", code.0)
}

/// 一条请求的 WinHTTP 句柄;Drop 按请求、连接、会话顺序关闭。
struct ResponseHandle {
    session: *mut c_void,
    connect: *mut c_void,
    request: *mut c_void,
}

impl ResponseHandle {
    fn open(
        request: &Request,
        secure: bool,
        host: &str,
        port: u16,
        path: &str,
    ) -> Result<Self, String> {
        if let Some((name, value)) = request
            .headers
            .iter()
            .find(|(name, value)| contains_break(name) || contains_break(value))
        {
            return Err(format!("请求头不能包含换行: {name}={value:?}"));
        }
        let agent = HSTRING::from("lueying/0.1");
        let host = HSTRING::from(host);
        let object = HSTRING::from(path);
        let verb = HSTRING::from(&request.method);
        let mut header_text = String::new();
        for (name, value) in &request.headers {
            header_text.push_str(name);
            header_text.push_str(": ");
            header_text.push_str(value);
            header_text.push_str("\r\n");
        }
        header_text.push_str("Accept-Encoding: identity\r\n");
        let headers: Vec<u16> = header_text.encode_utf16().collect();
        unsafe {
            let session = WinHttpOpen(
                &agent,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            );
            if session.is_null() {
                return Err(last_error("创建 WinHTTP 会话失败"));
            }
            let mut response = Self {
                session,
                connect: std::ptr::null_mut(),
                request: std::ptr::null_mut(),
            };
            WinHttpSetTimeouts(session, 5_000, 5_000, 10_000, request.timeout_ms as i32)
                .map_err(|error| format!("设置网络超时: {error}"))?;
            response.connect = WinHttpConnect(session, &host, port, 0);
            if response.connect.is_null() {
                return Err(last_error("连接服务器失败"));
            }
            let flags = if secure {
                WINHTTP_FLAG_SECURE
            } else {
                WINHTTP_OPEN_REQUEST_FLAGS(0)
            };
            response.request = WinHttpOpenRequest(
                response.connect,
                &verb,
                &object,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            );
            if response.request.is_null() {
                return Err(last_error("创建 HTTP 请求失败"));
            }
            let policy = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP.to_ne_bytes();
            let policy: &[u8] = &policy;
            WinHttpSetOption(
                Some(response.request as *const c_void),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(policy),
            )
            .map_err(|error| format!("设置重定向策略: {error}"))?;
            WinHttpAddRequestHeaders(response.request, &headers, WINHTTP_ADDREQ_FLAG_ADD)
                .map_err(|error| format!("设置请求头: {error}"))?;
            let (body, body_len): (Option<*const c_void>, u32) = match &request.body {
                Some(body) => (Some(body.as_ptr() as *const c_void), body.len() as u32),
                None => (None, 0),
            };
            WinHttpSendRequest(response.request, None, body, body_len, body_len, 0)
                .map_err(|error| format!("发送请求: {error}"))?;
            WinHttpReceiveResponse(response.request, std::ptr::null_mut())
                .map_err(|error| format!("读取响应: {error}"))?;
            Ok(response)
        }
    }

    fn status(&self) -> Result<u16, String> {
        let mut status = 0u32;
        let mut length = size_of::<u32>() as u32;
        let buffer: *mut c_void = (&mut status as *mut u32).cast();
        unsafe {
            WinHttpQueryHeaders(
                self.request,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PWSTR::null(),
                Some(buffer),
                &mut length,
                std::ptr::null_mut(),
            )
            .map_err(|error| format!("读取 HTTP 状态码: {error}"))?;
        }
        Ok(status as u16)
    }

    fn bytes(&self, limit: u64) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        self.read_chunks(limit, |chunk| {
            out.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(out)
    }

    fn read_chunks<F>(&self, limit: u64, mut on_chunk: F) -> Result<u64, String>
    where
        F: FnMut(&[u8]) -> Result<(), String>,
    {
        let mut total = 0u64;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            let pointer: *mut c_void = buffer.as_mut_ptr().cast();
            unsafe { WinHttpReadData(self.request, pointer, buffer.len() as u32, &mut read) }
                .map_err(|error| format!("读取响应数据: {error}"))?;
            if read == 0 {
                return Ok(total);
            }
            if total + u64::from(read) > limit {
                return Err(format!("响应内容超过 {limit} 字节"));
            }
            on_chunk(&buffer[..read as usize])?;
            total += u64::from(read);
        }
    }
}

fn contains_break(text: &str) -> bool {
    text.contains(['\r', '\n'])
}

impl Drop for ResponseHandle {
    fn drop(&mut self) {
        unsafe {
            if !self.request.is_null() {
                let _ = WinHttpCloseHandle(self.request);
            }
            if !self.connect.is_null() {
                let _ = WinHttpCloseHandle(self.connect);
            }
            if !self.session.is_null() {
                let _ = WinHttpCloseHandle(self.session);
            }
        }
    }
}
