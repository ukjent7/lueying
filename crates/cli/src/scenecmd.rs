//! `lueying scene` 的打包与安装:场景目录 ⇄ `.luext`。
//!
//! 场景包与扩展包共用 `lueying-pack v2` 容器(见
//! [`lueying_extmeta::container`]);包里必须有能解析的 `scene.toml`,
//! 文件按包内相对路径落进场景目录,SceneHost 一秒内热重载。

use std::fs;
use std::path::{Path, PathBuf};

use lueying_extmeta::container::{self, Files};

/// 场景包参与打包的后缀;其它后缀报错,宁可失败也不静默丢文件。
const PACK_EXTS: &[&str] = &[
    "toml", "json", "md", "txt", "png", "jpg", "jpeg", "webp", "gif", "wav", "mp3", "ogg",
];

/// `scene pack` 的参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScenePackArgs {
    pub dir: PathBuf,
    pub out: Option<PathBuf>,
}

/// `scene install` 的参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneInstallArgs {
    pub source: PathBuf,
    pub dir: Option<PathBuf>,
    pub force: bool,
}

/// 场景目录;测试与自定义安装位置用 `LUEYING_SCENE_DIR`。
pub fn scene_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LUEYING_SCENE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("lueying").join("scene"))
}

/// 打包一个场景目录;scene.toml 必须存在且能解析,素材随目录一起进包。
pub fn pack(args: &ScenePackArgs) -> Result<PathBuf, String> {
    let dir = &args.dir;
    if !dir.is_dir() {
        return Err(format!("{} 不是目录", dir.display()));
    }
    let scene_text =
        fs::read_to_string(dir.join("scene.toml")).map_err(|e| format!("读 scene.toml: {e}"))?;
    if let Err(errors) = lueying_scene::parse("scene.toml", &scene_text) {
        return Err(errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; "));
    }
    let mut paths = Vec::new();
    collect(dir, &mut paths)?;
    paths.sort();
    let mut files: Files = Vec::new();
    for path in &paths {
        let bytes = fs::read(path).map_err(|e| format!("读 {}: {e}", path.display()))?;
        let name = path
            .strip_prefix(dir)
            .expect("collect 只产出 dir 下的路径")
            .to_string_lossy()
            .replace('\\', "/");
        files.push((name, bytes));
    }
    let out = container::encode(&files)?;
    let target = args.out.clone().unwrap_or_else(|| {
        dir.with_file_name(format!(
            "{}.luext",
            dir.file_name().unwrap_or_default().to_string_lossy()
        ))
    });
    fs::write(&target, &out).map_err(|e| format!("写 {}: {e}", target.display()))?;
    Ok(target)
}

/// 安装一个场景包或目录到场景目录;默认不覆盖已有 scene.toml。
pub fn install(args: &SceneInstallArgs) -> Result<PathBuf, String> {
    let target_root = args
        .dir
        .clone()
        .or_else(scene_dir)
        .ok_or("找不到场景目录;用 --dir 指定,或设置 APPDATA")?;
    if !target_root.exists() {
        fs::create_dir_all(&target_root)
            .map_err(|e| format!("建 {}: {e}", target_root.display()))?;
    }
    let files = if args.source.is_dir() {
        read_directory(&args.source)?
    } else {
        let bytes =
            fs::read(&args.source).map_err(|e| format!("读 {}: {e}", args.source.display()))?;
        container::decode(&bytes)?
    };
    let scene = target_root.join("scene.toml");
    if scene.exists() && !args.force {
        return Err(format!("{} 已存在;加 --force 覆盖", scene.display()));
    }
    if !files.iter().any(|(name, _)| name == "scene.toml") {
        return Err("包里没有 scene.toml".to_string());
    }
    for (name, content) in &files {
        let path = target_root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("建 {}: {e}", parent.display()))?;
        }
        fs::write(&path, content).map_err(|e| format!("写 {}: {e}", path.display()))?;
    }
    // 装完再校验一次:坏包不留在场景目录里。
    let text = fs::read_to_string(&scene).map_err(|e| format!("读 scene.toml: {e}"))?;
    if let Err(errors) = lueying_scene::parse("scene.toml", &text) {
        let joined = errors
            .iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!("装好的 scene.toml 校验不过: {joined}"));
    }
    Ok(target_root)
}

fn read_directory(source: &Path) -> Result<Files, String> {
    let mut paths = Vec::new();
    collect(source, &mut paths)?;
    let mut files: Files = Vec::new();
    for path in paths {
        let relative = path
            .strip_prefix(source)
            .expect("collect 只产出 source 下的路径")
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&path).map_err(|e| format!("读 {}: {e}", path.display()))?;
        files.push((relative, bytes));
    }
    if !files.iter().any(|(name, _)| name == "scene.toml") {
        return Err(format!("{} 里没有 scene.toml", source.display()));
    }
    Ok(files)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("读 {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out)?;
            continue;
        }
        let kept = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| PACK_EXTS.contains(&ext));
        if kept {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "scenecmd_tests.rs"]
mod tests;
