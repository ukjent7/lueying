//! 配置同步:本机配置打包成一份载荷,经 WebDAV 在机器之间同步。
//!
//! 三块:
//! - 载荷:与设置窗「配置备份」同一份文本格式,能打包、解包与算摘要。
//! - 判定:记下上次同步成功的摘要,就能分清本机改了、远端改了还是两边都改。
//! - 传输:HTTPS + Basic 认证的 WebDAV 读写,住在设置窗那侧(见
//!   `settings_actions`),常驻服务不碰网络。
//!
//! 口令不进配置也不进载荷:地址与账号在 `config.toml`,口令放
//! `%APPDATA%\lueying\webdav-password.txt`,只留在本机。

use std::path::{Path, PathBuf};

use lueying_extmeta::sha256::{Sha256, hex};

use crate::config;

/// 载荷的文件头;解析时先认它。
pub const HEADER: &str = "### lueying-backup v1";

/// 远端文件名;地址是目录,文件跟在后面。
pub const FILE_NAME: &str = "lueying-config.txt";

/// 载荷体积上限(字节);远端文件超过就算异常,不整块读进内存。
pub const MAX_PAYLOAD: u64 = 4 * 1024 * 1024;

/// 打包:每块按行数声明长度,文件里出现什么标记都不影响解析。
pub fn pack(blocks: &[(String, String)]) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for (name, content) in blocks {
        let lines = content.lines().count();
        out.push_str(&format!("### begin: {name} {lines}\n"));
        if lines > 0 {
            out.push_str(content.trim_end_matches('\n'));
            out.push('\n');
        }
        out.push_str(&format!("### end: {name}\n"));
    }
    out
}

/// 解包:`### begin: <名字> <行数>` 之后逐行取声明数量的行,随后跳过
/// `### end:` 行;坏块整块跳过。
pub fn unpack(text: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("### begin: ") else {
            continue;
        };
        let Some((name, count)) = rest.rsplit_once(' ') else {
            continue;
        };
        let Ok(count) = count.parse::<usize>() else {
            continue;
        };
        let mut content = Vec::with_capacity(count);
        for _ in 0..count {
            match lines.next() {
                Some(line) => content.push(line.to_string()),
                None => break,
            }
        }
        lines.next_if(|line| line.starts_with("### end:"));
        files.push((name.to_string(), content.join("\n")));
    }
    files
}

/// 是否是本格式的载荷。
pub fn is_bundle(text: &str) -> bool {
    text.starts_with(HEADER)
}

/// 摘要:本机与远端摘要相同就是同一份。
pub fn digest(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex(&hasher.finish())
}

/// 远端地址:目录加文件名。
pub fn remote_url(base: &str) -> String {
    let trimmed = base.trim().trim_end_matches('/');
    format!("{trimmed}/{FILE_NAME}")
}

/// 地址是否可用:外部地址只允许 HTTPS,回环可以明文 HTTP。
/// 与 `lueying_net::http` 的约束一致,读配置时就能挡住坏地址。
pub fn valid_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() {
        return true;
    }
    if let Some(rest) = url.strip_prefix("https://") {
        return !authority(rest).is_empty();
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = host_of(authority(rest));
        return matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    }
    false
}

/// URL 里主机名之前的那一段。
fn authority(rest: &str) -> &str {
    rest.split(['/', '?', '#']).next().unwrap_or("")
}

/// 主机名;IPv6 的方括号与端口都去掉。
fn host_of(authority: &str) -> String {
    let host = if let Some(end) = authority.find(']') {
        &authority[1..end]
    } else {
        authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host)
    };
    host.trim_matches(['[', ']']).to_ascii_lowercase()
}

/// 该做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// 两边一样,不用动。
    UpToDate,
    /// 本机有改动,上传。
    Push,
    /// 远端有改动,下载。
    Pull,
    /// 两边都改过,交给用户选。
    Conflict,
}

/// 判定:远端没有就先上传;两边一样就是最新;跟上次的摘要比,谁变了就
/// 同步谁;两边都变或者没有上次记录,交给用户选。
pub fn decide(local: &str, remote: Option<&str>, last: Option<&str>) -> Action {
    let Some(remote) = remote else {
        return Action::Push;
    };
    if local == remote {
        return Action::UpToDate;
    }
    let local_digest = digest(local);
    let remote_digest = digest(remote);
    match last {
        Some(last) if last == local_digest => Action::Pull,
        Some(last) if last == remote_digest => Action::Push,
        _ => Action::Conflict,
    }
}

/// 口令文件路径;口令只留在本机。
pub fn password_path() -> Option<PathBuf> {
    config::webdav_password_path()
}

/// 读口令;没有文件就是空串。
pub fn read_password() -> String {
    password_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

/// 写口令;空串即删除。
pub fn write_password(password: &str) -> Result<(), String> {
    let path = password_path().ok_or("没有可写的口令文件路径")?;
    if password.trim().is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    write_atomic(&path, password.trim())
}

/// 上次同步成功的摘要;记在状态文件里,用来分辨谁改了。
pub fn read_last_digest() -> Option<String> {
    config::webdav_state_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// 写上次同步成功的摘要。
pub fn write_last_digest(digest: &str) -> Result<(), String> {
    let path = config::webdav_state_path().ok_or("没有可写的同步状态文件路径")?;
    write_atomic(&path, digest)
}

/// 先写临时文件再改名:中途失败不留半截文件。
fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("建目录失败: {error}"))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).map_err(|error| format!("写临时文件失败: {error}"))?;
    std::fs::rename(&tmp, path).map_err(|error| format!("改名失败: {error}"))
}

#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
