//! GitHub Release 自动更新:查询 latest、比较版本、WinHTTP 下载、SHA-256 校验。
//! Release 里带签名清单(`lueying-manifest.json` + `.sig`)时,镜像与哈希以
//! 验签后的清单为准(见 [`crate::update_manifest`]);设置窗把安装包下载到
//! 临时目录、校验后直接运行,由安装包完成替换。

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use lueying_protocol::version::{is_newer, parse_sha256};

use crate::http::Stream;
use crate::update_manifest::{self, MANIFEST_NAME};

/// 更新来源;发布工作流把 `lueying-<version>-setup.exe` 与 `.sha256` 传到这个仓库的 Release。
const API_LATEST: &str = "https://api.github.com/repos/ukjent7/lueying/releases/latest";
/// 测试渠道要看到预发布,latest 接口只回正式版,这里取最近 20 条。
const API_RELEASES: &str = "https://api.github.com/repos/ukjent7/lueying/releases?per_page=20";
const API_LIMIT: u64 = 2 * 1024 * 1024;
const SHA_LIMIT: u64 = 8 * 1024;
/// 签名文件是 64 字节 Ed25519 的十六进制,留足余量。
const SIG_LIMIT: u64 = 4 * 1024;

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub notes: String,
    pub installer_url: String,
    pub sha_url: String,
    /// GitHub 记录的资源字节数;0 表示不校验大小。
    pub size: u64,
    /// 签名清单给出的安装包哈希;有值时设置窗不再拉 `.sha256` 文件。
    pub sha256: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Check {
    Latest,
    Available(Release),
    /// 仓库还没有任何 Release。
    NoRelease,
}

/// 更新渠道;设置页的 `Channel` 在调用前映射成这里的两档。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
}

/// 安装目录里 `version.txt` 的版本;开发构建回退到 Cargo 版本。
pub fn current_version() -> String {
    std::fs::read_to_string(install_dir().join("version.txt"))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// 当前可执行文件所在目录。设置窗从这里读 `version.txt`。
pub fn install_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 按渠道查询更新。稳定渠道看 latest(不含预发布);测试渠道看最近
/// 20 条 Release(含预发布),取首个比当前新的——列表按时间倒序,
/// 首条就是 GitHub 上的最新版,版本号比较兜底乱序。
pub fn check_channel(channel: Channel) -> Result<Check, String> {
    match channel {
        Channel::Stable => {
            let Some((release, value)) = latest_release()? else {
                return Ok(Check::NoRelease);
            };
            available_or_latest(release, value)
        }
        Channel::Beta => {
            let releases = releases_list()?;
            let candidate = releases
                .iter()
                .find(|(release, _)| is_newer(&release.version, &current_version()))
                .map(|(release, value)| (release.clone(), value.clone()));
            Ok(match candidate {
                Some((release, value)) => Check::Available(attach_manifest(&value, release)?),
                None if releases.is_empty() => Check::NoRelease,
                None => Check::Latest,
            })
        }
    }
}

fn available_or_latest(release: Release, value: serde_json::Value) -> Result<Check, String> {
    if is_newer(&release.version, &current_version()) {
        Ok(Check::Available(attach_manifest(&value, release)?))
    } else {
        Ok(Check::Latest)
    }
}

fn latest_release() -> Result<Option<(Release, serde_json::Value)>, String> {
    let (status, body) = fetch(API_LATEST, API_LIMIT)?;
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(http_error(status));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("解析 GitHub 响应: {e}"))?;
    release_from_value(&json).map(|release| Some((release, json)))
}

/// 最近 20 条 Release;没有 setup.exe 资源的条目(纯源码发布)跳过。
fn releases_list() -> Result<Vec<(Release, serde_json::Value)>, String> {
    let (status, body) = fetch(API_RELEASES, API_LIMIT)?;
    if !(200..300).contains(&status) {
        return Err(http_error(status));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("解析 GitHub 响应: {e}"))?;
    let list = json
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|value| {
            release_from_value(value)
                .ok()
                .map(|release| (release, value.clone()))
        })
        .collect();
    Ok(list)
}

fn http_error(status: u16) -> String {
    if status == 403 {
        "GitHub 接口限流,请稍后再试".into()
    } else {
        format!("GitHub 接口返回 HTTP {status}")
    }
}

/// 从一条 Release JSON 里取版本与安装包资源;资源缺失时报错。
fn release_from_value(json: &serde_json::Value) -> Result<Release, String> {
    let tag = json["tag_name"]
        .as_str()
        .ok_or("GitHub 响应缺少 tag_name")?
        .to_string();
    let version = normalize_version(&tag);
    if version.is_empty() {
        return Err("GitHub Release 的 tag 为空".into());
    }
    let assets = json["assets"].as_array().cloned().unwrap_or_default();
    let setup_name = format!("lueying-{version}-setup.exe");
    let installer = assets
        .iter()
        .find(|asset| {
            asset
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(&setup_name))
        })
        .or_else(|| {
            assets.iter().find(|asset| {
                asset
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with("-setup.exe"))
            })
        })
        .ok_or_else(|| format!("Release {tag} 没有 setup.exe 安装包"))?;
    let installer_url = installer["browser_download_url"]
        .as_str()
        .ok_or("安装包缺少下载地址")?
        .to_string();
    let installer_name = installer["name"].as_str().unwrap_or(&setup_name);
    let sha_name = format!("{installer_name}.sha256");
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
    Ok(Release {
        version,
        tag,
        notes: json["body"].as_str().unwrap_or("").to_string(),
        installer_url,
        sha_url: sha["browser_download_url"]
            .as_str()
            .ok_or("校验文件缺少下载地址")?
            .to_string(),
        size: installer["size"].as_u64().unwrap_or(0),
        sha256: None,
    })
}

/// Release 资源里有签名清单时改用它:镜像、大小与哈希都来自验签后的内容,
/// 不再单独拉 `.sha256` 文件。清单在但验不过属于硬错误(被篡改或损坏的
/// 已签名资源,静默降级等于没签);清单缺失或拉取失败退回旧流程,
/// 兼容还没签名的历史 Release。
fn attach_manifest(value: &serde_json::Value, release: Release) -> Result<Release, String> {
    let assets = value["assets"].as_array().cloned().unwrap_or_default();
    let asset_url = |name: &str| {
        assets
            .iter()
            .find(|asset| {
                asset
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|asset| asset.eq_ignore_ascii_case(name))
            })
            .and_then(|asset| asset["browser_download_url"].as_str())
            .map(str::to_string)
    };
    let (Some(manifest_url), Some(sig_url)) = (
        asset_url(MANIFEST_NAME),
        asset_url(&format!("{MANIFEST_NAME}.sig")),
    ) else {
        return Ok(release);
    };
    let (status, manifest_body) = fetch(&manifest_url, update_manifest::MAX_MANIFEST_BYTES as u64)?;
    if !(200..300).contains(&status) {
        return Ok(release);
    }
    let (status, sig_body) = fetch(&sig_url, SIG_LIMIT)?;
    if !(200..300).contains(&status) {
        return Ok(release);
    }
    let signature =
        String::from_utf8(sig_body).map_err(|_| "签名文件不是 UTF-8 文本".to_string())?;
    let manifest = update_manifest::verify(&manifest_body, &signature)?;
    if manifest.release != release.version {
        return Err(format!(
            "清单版本 {} 与 Release 版本 {} 不一致",
            manifest.release, release.version
        ));
    }
    let file = manifest.installer(&release.version)?;
    Ok(Release {
        sha256: Some(file.sha256.clone()),
        size: file.size,
        installer_url: file.mirror()?.to_string(),
        ..release
    })
}

/// 把安装包下载到临时目录并校验;完成后由设置窗直接运行安装器。
///
/// 签名清单给过哈希就用它,否则拉 Release 的 `.sha256` 文件;写入前
/// 先卡大小,校验不过不留半截安装包。
pub fn download_installer(release: &Release) -> Result<PathBuf, String> {
    let dest = std::env::temp_dir().join(format!("lueying-{}-setup.exe", release.version));
    let _ = std::fs::remove_file(&dest);
    let limit = if release.size > 0 {
        release.size + 1024 * 1024
    } else {
        512 * 1024 * 1024
    };
    let (status, bytes) = fetch_bytes(&release.installer_url, limit)?;
    if !(200..300).contains(&status) {
        return Err(format!("下载安装包返回 HTTP {status}"));
    }
    if release.size > 0 && bytes.len() as u64 != release.size {
        return Err(format!(
            "安装包大小不符: {} != {}",
            bytes.len(),
            release.size
        ));
    }
    std::fs::write(&dest, &bytes).map_err(|e| format!("写 {}: {e}", dest.display()))?;
    let expected = match &release.sha256 {
        Some(sha) => sha.clone(),
        None => fetch_sha256(&release.sha_url)?,
    };
    if sha256_file(&dest)? != expected {
        let _ = std::fs::remove_file(&dest);
        return Err("安装包 SHA-256 校验失败".into());
    }
    Ok(dest)
}

pub fn download(url: &str, dest: &Path, expected: u64) -> Result<(), String> {
    let stream = Stream::open(url, 60_000)?;
    let status = stream.status();
    if !(200..300).contains(&status) {
        return Err(format!("下载更新包返回 HTTP {status}"));
    }
    // 清单带大小时按它卡上限;没带就给安装包留足余量。
    let limit = if expected > 0 {
        expected
    } else {
        512 * 1024 * 1024
    };
    let written = (|| -> Result<u64, String> {
        let mut file = File::create(dest).map_err(|e| format!("创建 {}: {e}", dest.display()))?;
        let written = stream.read_chunks(limit, |chunk| {
            file.write_all(chunk)
                .map_err(|e| format!("写入更新包: {e}"))
        })?;
        file.flush().map_err(|e| format!("写入更新包: {e}"))?;
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

/// 供官方扩展中心读取小文件(目录/校验文件)。返回 HTTP 状态与响应体。
pub fn fetch_bytes(url: &str, limit: u64) -> Result<(u16, Vec<u8>), String> {
    // 模型文件可能有几十 MB,用下载档的 60 秒接收超时。
    fetch_with(url, limit, true)
}

pub fn fetch_sha256(url: &str) -> Result<String, String> {
    let (status, body) = fetch(url, SHA_LIMIT)?;
    if !(200..300).contains(&status) {
        return Err(format!("下载 SHA-256 返回 HTTP {status}"));
    }
    let text = String::from_utf8_lossy(&body);
    parse_sha256(&text).ok_or_else(|| "SHA-256 文件格式不对".into())
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("打开 {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    lueying_ext::manifest::sha256::sha256_reader(&mut reader)
        .map_err(|e| format!("计算 SHA-256: {e}"))
}

fn normalize_version(tag: &str) -> String {
    let tag = tag.trim();
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

fn fetch(url: &str, limit: u64) -> Result<(u16, Vec<u8>), String> {
    fetch_with(url, limit, false)
}

fn fetch_with(url: &str, limit: u64, download: bool) -> Result<(u16, Vec<u8>), String> {
    let response = crate::http::send(&crate::http::Request {
        method: "GET".to_string(),
        url: url.to_string(),
        headers: vec![
            (
                "Accept".to_string(),
                "application/vnd.github+json".to_string(),
            ),
            ("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string()),
        ],
        body: None,
        limit,
        timeout_ms: if download { 60_000 } else { 15_000 },
        credentials: None,
    })?;
    Ok((response.status, response.body))
}
