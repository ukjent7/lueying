//! 扩展包落盘:设置窗导入、`lueying-settings ext install` 与 `ext add` 共用这一条路径。
//!
//! 同 id 已有目录就整体替换。先解包到临时目录再改名换位,任何一步失败旧目录都保持
//! 原样;替换后包内容变了,同意记录对不上哈希,扩展自动挂起等用户重新确认。

use std::path::{Path, PathBuf};

use lueying_ext::manifest::Manifest;
use lueying_ext::manifest::container::{self, Files};

/// 单个 `.luext` 的读入上限;容器对单文件与全包另有校验。
pub const MAX_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;

/// 把 `.luext` 字节落盘到扩展目录;返回给人看的一行说明。
pub fn install_luext_bytes(bytes: &[u8], root: &Path) -> Result<String, String> {
    let (manifest, _) = install_files(&container::decode(bytes)?, root)?;
    let kind = if manifest.is_native() {
        "原生扩展"
    } else {
        "扩展"
    };
    Ok(format!(
        "{kind} {} 已导入;在「运行期扩展」里同意后可用",
        manifest.name
    ))
}

/// 把包内文件落盘到 `root/<id>`;同 id 已有目录就整体替换,返回清单与安装目录。
///
/// 先写临时目录再改名换位,任何一步失败旧目录都保持原样。替换
/// 后包内容变了,同意记录对不上哈希,扩展自动挂起等用户重新确认。
pub fn install_files(files: &Files, root: &Path) -> Result<(Manifest, PathBuf), String> {
    let Some((_, manifest_bytes)) = files.iter().find(|(name, _)| name == "manifest.json") else {
        return Err("包里没有 manifest.json".to_string());
    };
    let manifest = Manifest::parse(manifest_bytes).map_err(|e| format!("清单不合法: {e}"))?;
    let payload = manifest.payload_file().to_string();
    if !payload.is_empty() && !files.iter().any(|(name, _)| *name == payload) {
        return Err(format!("包里缺少 {payload}"));
    }
    let staging = root.join(format!(".import-{}", manifest.id));
    let target = root.join(&manifest.id);
    let backup = root.join(format!(".backup-{}", manifest.id));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("建临时目录失败: {e}"))?;
    for (name, content) in files {
        let path = staging.join(name);
        if let Some(parent) = path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("建 {} 目录失败: {error}", parent.display()));
        }
        if let Err(error) = std::fs::write(&path, content) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(format!("写 {name} 失败: {error}"));
        }
    }
    if target.exists() && std::fs::rename(&target, &backup).is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("{} 可能正在运行,先停用或稍后再导入", manifest.id));
    }
    if let Err(error) = std::fs::rename(&staging, &target) {
        if backup.exists() {
            let _ = std::fs::rename(&backup, &target);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("落盘失败: {error}"));
    }
    let _ = std::fs::remove_dir_all(&backup);
    Ok((manifest, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(version: &str) -> Files {
        let manifest = format!(
            r#"{{"id":"demo","name":"演示","version":"{version}","api":4,"wasm":"extension.wasm"}}"#
        );
        vec![
            ("manifest.json".into(), manifest.into_bytes()),
            ("extension.wasm".into(), vec![0, 97, 115, 109]),
        ]
    }

    #[test]
    fn same_id_replaces_whole_directory() {
        let root = std::env::temp_dir().join(format!("lueying-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("建目录");
        install_files(&package("1.0.0"), &root).expect("首次安装");
        std::fs::write(root.join("demo").join("stale.txt"), b"x").expect("写旧文件");
        let (manifest, target) = install_files(&package("2.0.0"), &root).expect("覆盖安装");
        assert_eq!(manifest.version, "2.0.0");
        assert_eq!(target, root.join("demo"));
        assert!(!target.join("stale.txt").exists(), "旧目录整体替换");
        let _ = std::fs::remove_dir_all(&root);
    }
}
