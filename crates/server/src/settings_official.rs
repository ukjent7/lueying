//! 官方扩展/模型的安装动作:下载、校验、原子落盘。
//!
//! 设置窗只点一下按钮,真正的网络与文件操作在后台线程;完成后回窗口
//! 消息。目录里的每一项都带 SHA-256 与字节数,校验不过就不写目标文件。

use std::ffi::c_void;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::official_extensions::{self, OfficialExtension};

/// 安装完成消息;设置窗用它刷新目录与状态。
pub const WM_OFFICIAL_DONE: u32 = WM_APP + 4;

/// 后台安装结果:线程写,窗口线程取。
static RESULT: Mutex<Option<Result<String, String>>> = Mutex::new(None);

/// 启动后台安装;同一时间只允许装一项。
pub fn start(hwnd: HWND, id: String) {
    *RESULT.lock().unwrap_or_else(|poison| poison.into_inner()) = None;
    let raw = hwnd.0 as usize;
    let spawned = std::thread::Builder::new()
        .name("settings-official".into())
        .spawn(move || {
            let result = install(&id);
            *RESULT.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(result);
            unsafe {
                let hwnd = HWND(raw as *mut c_void);
                let _ = PostMessageW(Some(hwnd), WM_OFFICIAL_DONE, WPARAM(0), LPARAM(0));
            }
        });
    if let Err(error) = spawned {
        *RESULT.lock().unwrap_or_else(|poison| poison.into_inner()) =
            Some(Err(format!("启动安装线程失败: {error}")));
    }
}

/// 取走后台结果。
pub fn take_result() -> Option<Result<String, String>> {
    RESULT
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take()
}

/// 安装一项官方目录内容;返回给用户的完成说明。
pub fn install(id: &str) -> Result<String, String> {
    let extension = official_extensions::find(id).ok_or_else(|| format!("未知官方扩展 {id}"))?;
    if extension.is_extension() {
        install_bundled(extension)?;
        return Ok(format!("{} 已安装;启用前需要同意权限", extension.name));
    }
    install_model(extension)?;
    Ok(format!("{} 已下载并校验;重启输入法后生效", extension.name))
}

fn install_bundled(extension: &OfficialExtension) -> Result<(), String> {
    let relative = extension
        .bundled
        .as_deref()
        .ok_or_else(|| format!("{} 没有随包来源", extension.id))?;
    let source = bundled_source(relative);
    if !source.is_dir() {
        return Err(format!("安装目录缺少 {}", source.display()));
    }
    let target = crate::config::extensions_dir()
        .ok_or("没有可写的扩展目录")?
        .join(&extension.id);
    if target.exists() {
        return Err(format!("{} 已安装", extension.id));
    }
    copy_tree(&source, &target)?;
    let manifest = target.join("manifest.json");
    let bytes = std::fs::read(&manifest).map_err(|e| format!("读清单: {e}"))?;
    let parsed =
        lueying_extmeta::Manifest::parse(&bytes).map_err(|e| format!("清单不合法: {e}"))?;
    if parsed.id != extension.id {
        let _ = std::fs::remove_dir_all(&target);
        return Err(format!(
            "包 id {} 与目录项 {} 不一致",
            parsed.id, extension.id
        ));
    }
    Ok(())
}

/// 随包扩展在安装目录的 `extensions/`;开发构建回退仓库根目录。
fn bundled_source(relative: &str) -> PathBuf {
    let installed = lueying_net::update::install_dir().join(relative);
    if installed.exists() {
        return installed;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn install_model(extension: &OfficialExtension) -> Result<(), String> {
    let models = crate::config::models_dir().ok_or("没有可写的模型目录")?;
    let target_dir = if extension.id == "ppocr" {
        models.join("ppocr")
    } else {
        models
    };
    std::fs::create_dir_all(&target_dir).map_err(|e| format!("建模型目录: {e}"))?;
    if extension.id == "ppocr" {
        write_atomic(
            &target_dir.join("dict.txt"),
            official_extensions::PPOCR_DICT.as_bytes(),
        )?;
    }
    for file in &extension.files {
        let target = target_dir.join(&file.name);
        if target.is_file() && file_matches(&target, &file.sha256) {
            continue;
        }
        let limit = file.bytes.saturating_add(1024 * 1024);
        let (status, bytes) = lueying_net::update::fetch_bytes(&file.url, limit)?;
        if !(200..300).contains(&status) {
            return Err(format!("下载 {} 返回 HTTP {status}", file.name));
        }
        if bytes.len() as u64 != file.bytes {
            return Err(format!(
                "{} 大小不对: {} 字节,目录写的是 {}",
                file.name,
                bytes.len(),
                file.bytes
            ));
        }
        let actual = sha256(&bytes);
        if actual != file.sha256 {
            return Err(format!("{} 的 SHA-256 校验失败", file.name));
        }
        write_atomic(&target, &bytes)?;
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!(
        "{}.part",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("tmp")
    ));
    {
        let mut file =
            std::fs::File::create(&tmp).map_err(|e| format!("创建 {}: {e}", tmp.display()))?;
        file.write_all(bytes)
            .map_err(|e| format!("写 {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("同步 {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("改名 {}: {e}", path.display()))
}

fn file_matches(path: &Path, expected: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut reader = std::io::BufReader::new(file);
    let mut hash = lueying_extmeta::sha256::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hash.update(&buffer[..read]),
            Err(_) => return false,
        }
    }
    lueying_extmeta::sha256::hex(&hash.finish()) == expected
}

fn sha256(bytes: &[u8]) -> String {
    let mut hash = lueying_extmeta::sha256::Sha256::new();
    hash.update(bytes);
    lueying_extmeta::sha256::hex(&hash.finish())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("创建 {}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("读 {}: {e}", from.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let into = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &into)?;
        } else {
            std::fs::copy(&path, &into).map_err(|e| format!("复制 {}: {e}", path.display()))?;
        }
    }
    Ok(())
}
