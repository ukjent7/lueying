//! 云同步:把本机配置打成载荷,经 WebDAV 与其他机器互相同步。
//!
//! 载荷与「配置备份」同一份格式:配置、短语、标点、按应用配置与用户词库。
//! 同步地址与账号在配置里,口令在本机单独存;三者都不进载荷。

use crate::config::{self, AppConfig};
use crate::sync;
use lueying_protocol::Request;

use super::{call, reload, send};

/// 一次同步的结果。
#[derive(Clone, Debug)]
pub enum Outcome {
    /// 两边一样,没动。
    UpToDate,
    /// 已上传。
    Pushed,
    /// 已下载并落地。
    Pulled,
    /// 两边都改过;远端载荷在这里,交给用户裁决。
    Conflict(String),
}

/// 本机载荷:配置、短语、标点、按应用配置与用户词库。
///
/// 同步地址与账号只留在本机,不打进载荷;用户词库要常驻服务在跑才拿得到。
pub fn build_payload(cfg: &AppConfig) -> String {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut local = cfg.clone();
    local.webdav_url.clear();
    local.webdav_username.clear();
    if let Ok(text) = crate::config_file::render(&local) {
        blocks.push(("config.toml".to_string(), text));
    }
    for (name, path) in [
        ("phrases.txt", config::phrases_path()),
        ("punct.txt", config::punct_path()),
        ("apps.toml", config::apps_path()),
    ] {
        if let Some(path) = path
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            blocks.push((name.to_string(), text));
        }
    }
    if let Some(reply) = call(&Request::ExportUser) {
        blocks.push(("user.tsv".to_string(), reply.user));
    }
    sync::pack(&blocks)
}

/// 落地远端载荷:配置、短语、标点、按应用配置与用户词库。
///
/// 同步地址与账号保留本机的;词库并进常驻服务。
pub fn apply_payload(text: &str, cfg: &AppConfig) -> Result<usize, String> {
    if !sync::is_bundle(text) {
        return Err("远端文件不是掠影载荷".to_string());
    }
    let mut applied = 0usize;
    for (name, content) in sync::unpack(text) {
        match name.as_str() {
            "config.toml" => {
                let mut remote = AppConfig::default();
                let mut issues = Vec::new();
                let file: crate::config_file::FileConfig = toml::from_str(&content)
                    .map_err(|error| format!("远端配置解析失败: {error}"))?;
                crate::config_file::apply(&mut remote, file, "config.toml", &mut issues);
                remote.webdav_url = cfg.webdav_url.clone();
                remote.webdav_username = cfg.webdav_username.clone();
                let Some(path) = config::path() else {
                    return Err("没有可写的配置路径".to_string());
                };
                config::save(&path, &remote).map_err(|error| format!("写配置失败: {error}"))?;
            }
            "phrases.txt" | "punct.txt" | "apps.toml" => {
                let target = match name.as_str() {
                    "phrases.txt" => config::phrases_path(),
                    "punct.txt" => config::punct_path(),
                    _ => config::apps_path(),
                };
                let Some(path) = target else {
                    continue;
                };
                let body = if content.is_empty() {
                    String::new()
                } else {
                    format!("{content}\n")
                };
                std::fs::write(&path, body).map_err(|error| format!("写 {name} 失败: {error}"))?;
            }
            "user.tsv" => {
                send(&Request::ImportUser {
                    text: content.clone(),
                });
            }
            _ => continue,
        }
        applied += 1;
    }
    if applied == 0 {
        return Err("远端载荷里没有可用的内容".to_string());
    }
    reload();
    Ok(applied)
}

/// 一次同步要用的东西:远端凭据、载荷与本机配置。
pub struct SyncJob {
    /// 远端目录地址。
    pub url: String,
    /// Basic 认证账号。
    pub username: String,
    /// 口令。
    pub password: String,
    /// 本机载荷。
    pub payload: String,
    /// 本机配置;下载远端时保留本机的地址与账号。
    pub cfg: AppConfig,
}

/// 上传载荷;成功后记下摘要。
pub fn push(job: &SyncJob) -> Result<(), String> {
    let target = sync::remote_url(&job.url);
    put(&target, &job.username, &job.password, &job.payload)?;
    sync::write_last_digest(&sync::digest(&job.payload))
}

/// 下载并落地远端载荷;成功后记下摘要。
pub fn pull(job: &SyncJob, text: &str) -> Result<usize, String> {
    let applied = apply_payload(text, &job.cfg)?;
    sync::write_last_digest(&sync::digest(text))?;
    Ok(applied)
}

/// 取远端文件;远端没有(404)就是 None。
fn fetch(url: &str, username: &str, password: &str) -> Result<Option<String>, String> {
    let response = lueying_net::http::send(&lueying_net::http::Request {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: Vec::new(),
        body: None,
        limit: sync::MAX_PAYLOAD,
        timeout_ms: 30_000,
        credentials: Some((username.to_string(), password.to_string())),
    })?;
    match response.status {
        200..=299 => String::from_utf8(response.body)
            .map(Some)
            .map_err(|_| "远端文件不是文本".to_string()),
        404 => Ok(None),
        status => Err(format!("远端返回状态 {status}")),
    }
}

/// 写远端文件。
fn put(url: &str, username: &str, password: &str, body: &str) -> Result<(), String> {
    let response = lueying_net::http::send(&lueying_net::http::Request {
        method: "PUT".to_string(),
        url: url.to_string(),
        headers: vec![(
            "Content-Type".to_string(),
            "text/plain; charset=utf-8".to_string(),
        )],
        body: Some(body.as_bytes().to_vec()),
        limit: 64 * 1024,
        timeout_ms: 60_000,
        credentials: Some((username.to_string(), password.to_string())),
    })?;
    if (200..=299).contains(&response.status) {
        Ok(())
    } else {
        Err(format!("远端返回状态 {status}", status = response.status))
    }
}

/// 取远端、比摘要:该上传就上传,该下载就下载,两边都改过就交给用户。
pub fn sync_once(job: &SyncJob) -> Result<Outcome, String> {
    let target = sync::remote_url(&job.url);
    let remote = fetch(&target, &job.username, &job.password)?;
    let last = sync::read_last_digest();
    match sync::decide(&job.payload, remote.as_deref(), last.as_deref()) {
        sync::Action::UpToDate => Ok(Outcome::UpToDate),
        sync::Action::Push => {
            push(job)?;
            Ok(Outcome::Pushed)
        }
        sync::Action::Pull => {
            let remote = remote.ok_or("远端没有文件")?;
            pull(job, &remote)?;
            Ok(Outcome::Pulled)
        }
        sync::Action::Conflict => Ok(Outcome::Conflict(remote.unwrap_or_default())),
    }
}
