//! GitHub Release 自动更新:查询 latest、比较版本、WinHTTP 下载、SHA-256 校验、
//! 解压并校验发布包。更新器进程用这些函数完成实际安装。

use std::cmp::Ordering;
use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpAddRequestHeaders,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::core::{HSTRING, PWSTR};

/// 更新来源;发布工作流把 `lueying-<version>.zip` 与 `.sha256` 传到这个仓库的 Release。
const API_LATEST: &str = "https://api.github.com/repos/ukjent7/lueying/releases/latest";
const API_LIMIT: u64 = 2 * 1024 * 1024;
const SHA_LIMIT: u64 = 8 * 1024;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub notes: String,
    pub zip_url: String,
    pub sha_url: String,
    /// GitHub 记录的资源字节数;0 表示不校验大小。
    pub size: u64,
}

#[derive(Clone, Debug)]
pub enum Check {
    Latest,
    Available(Release),
    /// 仓库还没有任何 Release。
    NoRelease,
}

/// 安装目录里 `version.txt` 的版本;开发构建回退到 Cargo 版本。
pub fn current_version() -> String {
    std::fs::read_to_string(install_dir().join("version.txt"))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// 当前可执行文件所在目录。设置窗与更新器都从这里读 `version.txt`。
pub fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 安装目标目录,与安装器 `install_dir()` 使用同一套回退规则。
pub fn install_target() -> PathBuf {
    if let Some(dir) = std::env::var_os("LUEYING_INSTALL_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("ProgramFiles") {
        return PathBuf::from(dir).join("lueying");
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Programs")
        .join("lueying")
}

/// 查询 GitHub 最新 Release 并与本地版本比较。
pub fn check() -> Result<Check, String> {
    let Some(release) = latest_release()? else {
        return Ok(Check::NoRelease);
    };
    if is_newer(&release.version, &current_version()) {
        Ok(Check::Available(release))
    } else {
        Ok(Check::Latest)
    }
}

fn latest_release() -> Result<Option<Release>, String> {
    let (status, body) = fetch(API_LATEST, API_LIMIT)?;
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(if status == 403 {
            "GitHub 接口限流,请稍后再试".into()
        } else {
            format!("GitHub 接口返回 HTTP {status}")
        });
    }
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("解析 GitHub 响应: {e}"))?;
    let tag = json["tag_name"]
        .as_str()
        .ok_or("GitHub 响应缺少 tag_name")?
        .to_string();
    let version = normalize_version(&tag);
    if version.is_empty() {
        return Err("GitHub Release 的 tag 为空".into());
    }
    let assets = json["assets"].as_array().cloned().unwrap_or_default();
    let zip_name = format!("lueying-{version}.zip");
    let zip = assets
        .iter()
        .find(|asset| {
            asset
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(&zip_name))
        })
        .or_else(|| {
            assets.iter().find(|asset| {
                asset
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with(".zip"))
            })
        })
        .ok_or_else(|| format!("Release {tag} 没有 zip 更新包"))?;
    let zip_url = zip["browser_download_url"]
        .as_str()
        .ok_or("zip 更新包缺少下载地址")?
        .to_string();
    let zip_asset_name = zip["name"].as_str().unwrap_or(&zip_name);
    let sha_name = format!("{zip_asset_name}.sha256");
    let sha = assets
        .iter()
        .find(|asset| {
            asset
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(&sha_name))
        })
        .or_else(|| {
            assets.iter().find(|asset| {
                asset
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with(".sha256"))
            })
        })
        .ok_or_else(|| format!("Release {tag} 没有 .sha256 校验文件"))?;
    Ok(Some(Release {
        version,
        tag,
        notes: json["body"].as_str().unwrap_or("").to_string(),
        zip_url,
        sha_url: sha["browser_download_url"]
            .as_str()
            .ok_or("校验文件缺少下载地址")?
            .to_string(),
        size: zip["size"].as_u64().unwrap_or(0),
    }))
}

/// 把设置进程里的更新器复制到临时目录再启动,避免安装时覆盖正在运行的 exe。
pub fn launch_updater(release: &Release, settings_pid: u32) -> Result<(), String> {
    let source = install_dir().join("lueying-update.exe");
    if !source.is_file() {
        return Err("安装目录缺少 lueying-update.exe,请重新安装".into());
    }
    let dir = std::env::temp_dir().join(format!("lueying-update-{settings_pid}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建更新临时目录: {e}"))?;
    let exe = dir.join("lueying-update.exe");
    if let Err(error) = std::fs::copy(&source, &exe) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("复制更新器: {error}"));
    }
    let size = release.size.to_string();
    let pid = settings_pid.to_string();
    let spawned = Command::new(&exe)
        .current_dir(&dir)
        .args([
            "--url",
            release.zip_url.as_str(),
            "--sha-url",
            release.sha_url.as_str(),
            "--version",
            release.version.as_str(),
            "--size",
            size.as_str(),
            "--wait-pid",
            pid.as_str(),
        ])
        .spawn();
    if let Err(error) = spawned {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("启动更新器: {error}"));
    }
    Ok(())
}

/// 等设置进程退出,让安装器能替换设置窗 exe;进程已退出视为成功。
pub fn wait_for_process(pid: u32, timeout_ms: u32) -> Result<(), String> {
    unsafe {
        let process = match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(process) => process,
            Err(_) => return Ok(()),
        };
        let waited = WaitForSingleObject(process, timeout_ms);
        let _ = CloseHandle(process);
        if waited == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err("等待设置窗口退出超时".into())
        }
    }
}

pub fn download(url: &str, dest: &Path, expected: u64) -> Result<(), String> {
    let response = Response::open(url, true)?;
    let status = response.status()?;
    if !(200..300).contains(&status) {
        return Err(format!("下载更新包返回 HTTP {status}"));
    }
    let written = (|| -> Result<u64, String> {
        let mut file =
            File::create(dest).map_err(|e| format!("创建 {}: {e}", dest.display()))?;
        let written = response.read_chunks(|chunk| {
            file.write_all(chunk)
                .map_err(|e| format!("写入更新包: {e}"))
        })?;
        file.flush()
            .map_err(|e| format!("写入更新包: {e}"))?;
        Ok(written)
    })();
    match written {
        Ok(written) => {
            if expected > 0 && written != expected {
                let _ = std::fs::remove_file(dest);
                return Err(format!("更新包大小不符: {written} != {expected}"));
            }
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_file(dest);
            Err(error)
        }
    }
}

pub fn fetch_sha256(url: &str) -> Result<String, String> {
    let (status, body) = fetch(url, SHA_LIMIT)?;
    if !(200..300).contains(&status) {
        return Err(format!("下载 SHA-256 返回 HTTP {status}"));
    }
    let text = String::from_utf8_lossy(&body);
    parse_sha(&text).ok_or_else(|| "SHA-256 文件格式不对".into())
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let output = Command::new(system32("certutil.exe"))
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("运行 certutil: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "certutil 退出码 {:?}: {}",
            output.status.code(),
            tail(&output.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_sha(&text).ok_or_else(|| "certutil 输出里没有 SHA-256".into())
}

/// 解压 zip。优先用系统自带的 bsdtar(Windows 10 1803+),失败再退回 PowerShell。
pub fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("创建解压目录: {e}"))?;
    let tar = system32("tar.exe");
    let tar_detail = match Command::new(&tar)
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(dest)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(output) if output.status.success() => return Ok(()),
        Ok(output) => format!("tar: {}", tail(&output.stderr)),
        Err(error) => format!("tar: {error}"),
    };
    let powershell = system32(r"WindowsPowerShell\v1.0\powershell.exe");
    let script = format!(
        "$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath {} -DestinationPath {} -Force",
        ps_quote(zip),
        ps_quote(dest)
    );
    let detail = match Command::new(&powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script.as_str(),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(output) if output.status.success() => return Ok(()),
        Ok(output) => format!("PowerShell: {}", tail(&output.stderr)),
        Err(error) => format!("PowerShell: {error}"),
    };
    Err(format!("解压更新包失败({tar_detail}; {detail})"))
}

/// 校验解压后的发布包:版本、必需二进制、版本化 TSF DLL 与清单文件大小。
pub fn validate_package(dir: &Path, version: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(dir.join("version.txt"))
        .map_err(|e| format!("更新包缺少 version.txt: {e}"))?;
    if text.trim() != version {
        return Err(format!(
            "更新包版本 {} 与期望 {version} 不一致",
            text.trim()
        ));
    }
    for name in [
        "lueying-install.exe",
        "lueying-server.exe",
        "lueying-settings.exe",
        "lueying-update.exe",
    ] {
        if !dir.join(name).is_file() {
            return Err(format!("更新包缺少 {name}"));
        }
    }
    let dll = format!("lueying_tsf-{}.dll", safe_version(version));
    if !dir.join(&dll).is_file() {
        return Err(format!("更新包缺少 {dll}"));
    }
    let manifest = std::fs::read(dir.join("manifest.json"))
        .map_err(|e| format!("更新包缺少 manifest.json: {e}"))?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest).map_err(|e| format!("更新包 manifest 无效: {e}"))?;
    if manifest["version"].as_str() != Some(version) {
        return Err("更新包 manifest 版本与目录不一致".into());
    }
    let files = manifest["files"]
        .as_array()
        .ok_or("更新包 manifest 缺少 files")?;
    for entry in files {
        let name = entry["path"]
            .as_str()
            .ok_or("更新包 manifest 文件项缺少 path")?;
        let path = Path::new(name);
        let unsafe_path = path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        if unsafe_path {
            return Err(format!("更新包 manifest 含不安全路径 {name}"));
        }
        let metadata =
            std::fs::metadata(dir.join(path)).map_err(|e| format!("更新包缺少 {name}: {e}"))?;
        if let Some(bytes) = entry["bytes"].as_u64() {
            if metadata.len() != bytes {
                return Err(format!(
                    "更新包 {name} 大小不符: {} != {bytes}",
                    metadata.len()
                ));
            }
        }
    }
    Ok(())
}

/// 版本比较(core 数字段 + 预发布标识),供检查更新与单元测试使用。
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

fn normalize_version(tag: &str) -> String {
    let tag = tag.trim();
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

fn safe_version(version: &str) -> String {
    version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn parse_sha(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        // sha256sum 格式是 `<hash>  <file>`;certutil 有的版本把摘要按 2 位一组
        // 用空格分开,行上只有摘要。两种都接受。
        if let Some(token) = line.split_whitespace().next() {
            if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Some(token.to_ascii_lowercase());
            }
        }
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        (compact.len() == 64 && compact.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| compact.to_ascii_lowercase())
    })
}

fn fetch(url: &str, limit: u64) -> Result<(u16, Vec<u8>), String> {
    let response = Response::open(url, false)?;
    let status = response.status()?;
    let body = response.bytes(limit)?;
    Ok((status, body))
}

fn system32(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join(name)
}

fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

fn tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(600);
    String::from_utf8_lossy(&bytes[start..]).trim().to_string()
}

fn last_error(what: &str) -> String {
    let code = unsafe { GetLastError() };
    format!("{what}(Win32 错误 {})", code.0)
}

/// 一条 GET 请求的 WinHTTP 句柄;Drop 按请求、连接、会话顺序关闭。
struct Response {
    session: *mut c_void,
    connect: *mut c_void,
    request: *mut c_void,
}

impl Response {
    fn open(url: &str, download: bool) -> Result<Self, String> {
        let (host, path) = split_https(url)?;
        let agent = HSTRING::from(format!("lueying-update/{}", env!("CARGO_PKG_VERSION")));
        let host = HSTRING::from(host);
        let object = HSTRING::from(path);
        let verb = HSTRING::from("GET");
        // User-Agent 由 WinHttpOpen 的 agent 统一发送,请求头里不再重复加。
        let headers: Vec<u16> = concat!(
            "Accept: application/vnd.github+json\r\n",
            "Accept-Encoding: identity\r\n",
            "X-GitHub-Api-Version: 2022-11-28\r\n",
        )
        .encode_utf16()
        .collect();
        unsafe {
            let session = WinHttpOpen(
                &agent,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PWSTR::null(),
                PWSTR::null(),
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
            let receive_timeout = if download { 60_000 } else { 15_000 };
            WinHttpSetTimeouts(session, 5_000, 5_000, 10_000, receive_timeout)
                .map_err(|e| format!("设置网络超时: {e}"))?;
            response.connect = WinHttpConnect(session, &host, 443, 0);
            if response.connect.is_null() {
                return Err(last_error("连接 GitHub 失败"));
            }
            response.request = WinHttpOpenRequest(
                response.connect,
                &verb,
                &object,
                PWSTR::null(),
                PWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
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
            .map_err(|e| format!("设置重定向策略: {e}"))?;
            WinHttpAddRequestHeaders(response.request, &headers, WINHTTP_ADDREQ_FLAG_ADD)
                .map_err(|e| format!("设置请求头: {e}"))?;
            WinHttpSendRequest(response.request, None, None, 0, 0, 0)
                .map_err(|e| format!("发送请求: {e}"))?;
            WinHttpReceiveResponse(response.request, std::ptr::null_mut())
                .map_err(|e| format!("读取响应: {e}"))?;
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
            .map_err(|e| format!("读取 HTTP 状态码: {e}"))?;
        }
        Ok(status as u16)
    }

    fn bytes(&self, limit: u64) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        self.read_chunks(|chunk| {
            let total = out.len() as u64 + chunk.len() as u64;
            if total > limit {
                return Err("响应内容过大".into());
            }
            out.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(out)
    }

    fn read_chunks(
        &self,
        mut sink: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, String> {
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let mut read = 0u32;
            let pointer: *mut c_void = buffer.as_mut_ptr().cast();
            unsafe {
                WinHttpReadData(self.request, pointer, buffer.len() as u32, &mut read)
            }
            .map_err(|e| format!("读取下载数据: {e}"))?;
            if read == 0 {
                return Ok(total);
            }
            total += u64::from(read);
            sink(&buffer[..read as usize])?;
        }
    }
}

impl Drop for Response {
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

fn split_https(url: &str) -> Result<(&str, &str), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("只支持 HTTPS 地址: {url}"))?;
    let (host, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if host.is_empty() {
        return Err(format!("地址缺少主机名: {url}"));
    }
    Ok((host, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_release_versions() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(is_newer("0.2.0-beta.2", "0.2.0-beta.1"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
    }

    #[test]
    fn reads_sha256_from_certutil_output() {
        let text = "SHA256 hash of file C:\\tmp\\lueying.zip:\n\
                    d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592\n\
                    CertUtil: -hashfile command completed successfully.\n";
        assert_eq!(
            parse_sha(text).as_deref(),
            Some("d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592")
        );
    }
}
