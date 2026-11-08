//! `lueying ext add`:从 GitHub 拉社区扩展,一条命令装好。
//!
//! 源写作 `github:<作者>/<仓库>#<版本>`,版本是 tag 或提交号,必填——装
//! 的就是钉住的那一版。取货按顺序试两条路:
//!
//! 1. GitHub Release 资产:该版本的 Release 附带的 `.luext` 包(作者流水线
//!    的产物,`ext pack` 的标准产物,首选)。
//! 2. 仓库源码包:版本指向的目录树里直接提交了构建好的组件——清单在仓库
//!    根,或 `extensions/<id>/` 布局里恰有一个。
//!
//! 信任锚是钉住的版本本身:同一版本重复 `add`,拿到的内容一样。装上之后
//! 首次启用仍要过设置页的同意流程,内容指纹由同意记录记死;本模块只管
//! 拉取与落盘,不加载 Wasm,Windows 与 Linux 都能跑。

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lueying_extmeta::Manifest;

use crate::extcmd::{self, InstallArgs};

/// 清单应答的体积上限。
const JSON_LIMIT: u64 = 1 << 20;
/// Release 资产(`.luext`)的体积上限。
const ASSET_LIMIT: u64 = 64 << 20;
/// 源码包的下载上限。
const TARBALL_LIMIT: u64 = 96 << 20;
/// 源码包解包后的总量上限。
const UNPACKED_LIMIT: u64 = 256 << 20;
/// 源码包允许的条目数上限。
const ENTRY_LIMIT: usize = 8192;
/// GitHub 对无 UA 的请求直接 403。
const USER_AGENT: &str = "lueying-cli (ext add)";

/// `ext add` 的参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddArgs {
    /// 来源,形如 `github:作者/仓库#版本`。
    pub source: String,
    /// 扩展根目录;缺省用 [`extensions_dir`](extcmd::extensions_dir)。
    pub dir: Option<PathBuf>,
    /// 同名包已存在时覆盖。
    pub force: bool,
}

/// 解析并安装一个社区扩展;返回给人看的汇报。
pub fn add(args: &AddArgs) -> Result<String, String> {
    let (owner, repo, rev) = parse_source(&args.source)?;
    let agent = agent();

    // 路径一:该版本的 GitHub Release 里有没有 `.luext` 资产。
    let release = release_json(&agent, owner, repo, rev)?;
    let asset = match &release {
        Some(value) => pick_asset(repo, value)?,
        None => None,
    };
    let temp = TempDir::new("add")?;
    let (origin, packed, packed_path) = match asset {
        Some((url, _size)) => {
            let bytes = download(&agent, &url, ASSET_LIMIT)?;
            let path = temp.0.join(format!("{repo}.luext"));
            fs::write(&path, &bytes).map_err(|e| format!("写临时包: {e}"))?;
            ("GitHub Release 资产", bytes, path)
        }
        None => {
            // 路径二:源码包;两条路都断时把修法一并说清。
            let bytes = source_tarball(&agent, owner, repo, rev).map_err(|error| {
                format!(
                    "Release 里没有 .luext 资产,源码包路径也失败:{error}\n\
                     作者侧修法:发布时附上 .luext 资产,或在仓库里提交构建好的组件 \
                     (见 docs/extensions/catalog.md)"
                )
            })?;
            let unpacked = unpack_tarball(&bytes, &temp.0)?;
            let picked = pick_extension_dir(&unpacked)?;
            let source = align_dir_name(&picked, &temp.0)?;
            ("仓库源码包", bytes, source)
        }
    };
    let fingerprint = lueying_extmeta::sha256::hex(&hash(&packed));

    let target = extcmd::install(&InstallArgs {
        source: packed_path,
        dir: args.dir.clone(),
        force: args.force,
    })?;
    let manifest_bytes =
        fs::read(target.join("manifest.json")).map_err(|e| format!("读清单: {e}"))?;
    let manifest = Manifest::parse(&manifest_bytes).map_err(|e| format!("清单不合法: {e}"))?;
    Ok(format!(
        "来源: github:{owner}/{repo}#{rev}({origin})\nsha256: {fingerprint}\n\
         已安装 {};首次启用要在 设置 → 扩展 里同意:\n{}",
        target.display(),
        summary(&manifest)
    ))
}

/// 解析 `github:作者/仓库#版本`;各段字符收紧,拼 URL 的输入不做透传。
pub fn parse_source(source: &str) -> Result<(&str, &str, &str), String> {
    let rest = source
        .strip_prefix("github:")
        .ok_or("来源要写成 github:作者/仓库#版本")?;
    let (slug, rev) = rest.split_once('#').ok_or("缺 #版本;要钉住装哪一版")?;
    let slug = slug.trim();
    let rev = rev.trim();
    if !safe_token(rev) || rev == ".." {
        return Err(format!("版本 {rev:?} 不合法:只允许字母、数字与 . _ -"));
    }
    let (owner, repo) = slug.split_once('/').ok_or("仓库要写成 作者/仓库")?;
    if owner.is_empty() || !safe_token(owner) || owner.starts_with('.') {
        return Err(format!("作者名 {owner:?} 不合法"));
    }
    if repo.is_empty() || !safe_token(repo) || repo.starts_with('.') {
        return Err(format!("仓库名 {repo:?} 不合法"));
    }
    Ok((owner, repo, rev))
}

/// URL 安全字符:字母、数字、点、下划线、连字符。
fn safe_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 一个带整体超时的 HTTP 客户端。
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .new_agent()
}

/// 一个 GET;2xx 的响应体按上限读回,超限即错。
fn download(agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(code) => format!("HTTP {code} 来自 {url}"),
            other => format!("请求 {url} 失败: {other}"),
        })?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读 {url}: {e}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{url} 的响应超过 {limit} 字节上限"));
    }
    Ok(bytes)
}

/// 查该版本的 Release;没有 Release(404)返回 `None`,限流单独点出来。
fn release_json(
    agent: &ureq::Agent,
    owner: &str,
    repo: &str,
    rev: &str,
) -> Result<Option<serde_json::Value>, String> {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/tags/{rev}");
    let bytes = match agent
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(mut response) => {
            let mut bytes = Vec::new();
            response
                .body_mut()
                .by_ref()
                .take(JSON_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("读 {url}: {e}"))?;
            if bytes.len() as u64 > JSON_LIMIT {
                return Err(format!("{url} 的响应超过 {JSON_LIMIT} 字节上限"));
            }
            bytes
        }
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(ureq::Error::StatusCode(403)) => {
            return Err(format!("GitHub API 限流(403);稍后再试,或直接看 {url}"));
        }
        Err(ureq::Error::StatusCode(code)) => return Err(format!("HTTP {code} 来自 {url}")),
        Err(other) => return Err(format!("请求 {url} 失败: {other}")),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("Release 应答不是 JSON: {e}"))
}

/// 从 Release 应答里挑 `.luext` 资产;多个时认 `{仓库名}.luext`,否则拒绝。
pub fn pick_asset(
    repo: &str,
    release: &serde_json::Value,
) -> Result<Option<(String, u64)>, String> {
    let Some(assets) = release.get("assets").and_then(|value| value.as_array()) else {
        return Err("Release 应答里没有 assets".into());
    };
    let mut found = Vec::new();
    for asset in assets {
        let name = asset.get("name").and_then(|value| value.as_str()).unwrap_or("");
        if name.to_ascii_lowercase().ends_with(".luext") {
            let url = asset
                .get("browser_download_url")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let size = asset.get("size").and_then(|value| value.as_u64()).unwrap_or(0);
            found.push((name.to_string(), url, size));
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => {
            let (_, url, size) = found.remove(0);
            Ok(Some((url, size)))
        }
        _ => {
            let preferred = format!("{repo}.luext");
            if let Some((_, url, size)) = found
                .iter()
                .find(|(name, _, _)| name.eq_ignore_ascii_case(&preferred))
            {
                return Ok(Some((url.clone(), *size)));
            }
            let names = found
                .into_iter()
                .map(|(name, _, _)| name)
                .collect::<Vec<_>>()
                .join("、");
            Err(format!(
                "Release 里有多个 .luext 资产:{names};一个 Release 只带一个包"
            ))
        }
    }
}

/// 拉该版本的源码包(codeload 的 tar.gz)。
fn source_tarball(
    agent: &ureq::Agent,
    owner: &str,
    repo: &str,
    rev: &str,
) -> Result<Vec<u8>, String> {
    download(agent, &format!("https://codeload.github.com/{owner}/{repo}/tar.gz/{rev}"), TARBALL_LIMIT)
}

/// 解包 tar.gz 到临时目录;只收普通文件与目录,路径越界一律拒绝。
fn unpack_tarball(packed: &[u8], dest: &Path) -> Result<PathBuf, String> {
    let gz = flate2::read::GzDecoder::new(packed);
    let mut archive = tar::Archive::new(gz);
    let raw = archive
        .entries()
        .map_err(|e| format!("源码包不是合法的 tar.gz: {e}"))?;
    let mut total = 0u64;
    let mut count = 0usize;
    for entry in raw {
        let mut entry = entry.map_err(|e| format!("读源码包: {e}"))?;
        count += 1;
        if count > ENTRY_LIMIT {
            return Err(format!("源码包条目超过 {ENTRY_LIMIT},不像一个扩展仓库"));
        }
        let path = &entry.path().map_err(|e| format!("读条目路径: {e}"))?;
        let Some(relative) = safe_relative(path) else {
            return Err("源码包里有越界路径,拒绝解包".into());
        };
        let target = dest.join(&relative);
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                fs::create_dir_all(&target).map_err(|e| format!("建 {relative}: {e}"))?;
            }
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| format!("建 {relative}: {e}"))?;
                }
                total += entry.size();
                if total > UNPACKED_LIMIT {
                    return Err(format!("源码包解包总量超过 {UNPACKED_LIMIT} 字节"));
                }
                let mut bytes = Vec::new();
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|e| format!("读 {relative}: {e}"))?;
                fs::write(&target, &bytes).map_err(|e| format!("写 {relative}: {e}"))?;
            }
            // 链接与特殊文件一律跳过:GitHub 源码包里本来就只有目录与文件。
            _ => {}
        }
    }
    if count == 0 {
        return Err("源码包是空的".into());
    }
    Ok(dest.to_path_buf())
}

/// tar 条目路径换相对路径;带 `..`、绝对路径或盘符的一律 `None`。
pub fn safe_relative(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                let part = part.to_str()?;
                if part.is_empty() {
                    return None;
                }
                parts.push(part);
            }
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// 定位扩展目录:仓库根的 manifest.json,或 `extensions/` 下恰有一个。
pub fn pick_extension_dir(root: &Path) -> Result<PathBuf, String> {
    if root.join("manifest.json").is_file() {
        return Ok(root.to_path_buf());
    }
    let extensions = root.join("extensions");
    let mut found = Vec::new();
    if extensions.is_dir() {
        for entry in
            fs::read_dir(&extensions).map_err(|e| format!("读 {}: {e}", extensions.display()))?
        {
            let path =
                entry.map_err(|e| format!("读 {}: {e}", extensions.display()))?.path();
            if path.join("manifest.json").is_file() {
                found.push(path);
            }
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(
            "仓库树里没有找到扩展:根目录或 extensions/<id>/ 下要有 manifest.json, \
             并且提交构建好的 extension.wasm"
                .into(),
        ),
        _ => {
            let mut ids = Vec::new();
            for path in &found {
                if let Ok(bytes) = fs::read(path.join("manifest.json"))
                    && let Ok(manifest) = Manifest::parse(&bytes)
                {
                    ids.push(manifest.id);
                }
            }
            Err(format!(
                "仓库里有多个扩展({});一个仓库只放一个,或只提交一个构建产物",
                ids.join("、")
            ))
        }
    }
}

/// 安装要求目录名等于清单 id;解包目录名不凑巧时搬成清单 id。
fn align_dir_name(picked: &Path, temp_root: &Path) -> Result<PathBuf, String> {
    let manifest_bytes =
        fs::read(picked.join("manifest.json")).map_err(|e| format!("读清单: {e}"))?;
    let manifest = Manifest::parse(&manifest_bytes).map_err(|e| format!("清单不合法: {e}"))?;
    if picked.file_name().and_then(|name| name.to_str()) == Some(manifest.id.as_str()) {
        return Ok(picked.to_path_buf());
    }
    let target = temp_root.join("install").join(&manifest.id);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("建 {}: {e}", parent.display()))?;
    }
    fs::rename(picked, &target).map_err(|e| format!("整理扩展目录: {e}"))?;
    Ok(target)
}

/// 给人看的清单摘要:名称版本、命令、服务、权限与联网白名单。
pub fn summary(manifest: &Manifest) -> String {
    let mut lines = vec![format!(
        "  {} {}(id {})",
        manifest.name, manifest.version, manifest.id
    )];
    if let Some(activate) = &manifest.activate
        && !activate.commands.is_empty()
    {
        let commands = activate
            .commands
            .iter()
            .map(|command| command.id.as_str())
            .collect::<Vec<_>>()
            .join("、");
        lines.push(format!("  命令: {commands}"));
    }
    if !manifest.needs.is_empty() {
        let needs = manifest
            .needs
            .iter()
            .map(|service| service.key())
            .collect::<Vec<_>>()
            .join("、");
        lines.push(format!("  宿主服务: {needs}"));
    }
    if !manifest.capabilities.is_empty() {
        lines.push(format!("  权限: {}", manifest.capabilities.join("、")));
    }
    if let Some(network) = &manifest.network {
        lines.push(format!("  联网白名单: {}", network.allow.join("、")));
    }
    if !manifest.settings.is_empty() {
        lines.push(format!("  设置项: {} 个", manifest.settings.len()));
    }
    lines.join("\n")
}

/// 包内容的 SHA-256。
fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = lueying_extmeta::sha256::Sha256::new();
    hasher.update(bytes);
    hasher.finish()
}

/// 进程内唯一的临时目录;用完即删。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Result<Self, String> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "lueying-ext-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).map_err(|e| format!("建临时目录: {e}"))?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
#[path = "extadd_tests.rs"]
mod tests;
