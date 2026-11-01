//! 设置窗的平台侧动作:管道、文件对话框、备份与自动更新。
//!
//! 页面与配置改动在 [`crate::settings_view`];这里只做「要动文件系统、
//! 要问常驻服务」的事,以及那些离不开 Windows API 的对话框。

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{self, AppOverride};
use crate::settings_view::{ConsentState, Notice, ScriptEntry, Settings, status_detail};
use lueying_extmeta::metrics::ExtMetrics;
use lueying_extwasm::{ConsentBook, ConsentCheck};
use lueying_net::update;
use lueying_protocol::{Reply, Request, pipe_name};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IDOK,
    IsWindowVisible, MB_DEFBUTTON2, MB_ICONQUESTION, MB_OKCANCEL, MessageBoxW,
};
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};

/// 给常驻 Server 发一条请求并取回应答;没在运行返回 None。
pub fn call(request: &Request) -> Option<Reply> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name())
        .ok()?;
    let frame = lueying_protocol::encode(request).ok()?;
    file.write_all(&frame).and_then(|()| file.flush()).ok()?;
    let mut reader = BufReader::new(file);
    let body = lueying_protocol::read_frame(&mut reader).ok()?;
    lueying_protocol::decode::<Reply>(&body).ok()
}

/// 给常驻 Server 发一条请求;没在运行返回 false。
pub fn send(request: &Request) -> bool {
    call(request).is_some()
}

/// 通知常驻 Server 重读配置。
pub fn reload() {
    let _ = send(&Request::ReloadConfig);
}

#[path = "settings_backup.rs"]
mod backup;

pub use backup::{backup_export, backup_import};

/// 存盘:写配置并让常驻服务重读。
pub fn save(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(path) = settings.path.clone() else {
        settings.say(Notice::Error, "没有可写的配置路径");
        return;
    };
    let dropped = crate::settings_ext::normalize(&mut settings.cfg, &settings.scripts);
    match config::save(&path, &settings.cfg) {
        Ok(()) => {
            settings.cfg.config_issues.clear();
            reload();
            if dropped == 0 {
                settings.say(Notice::Success, "已保存");
            } else {
                settings.say(
                    Notice::Info,
                    format!("已保存;丢弃 {dropped} 个清单外的扩展设置"),
                );
            }
        }
        Err(e) => settings.say(Notice::Error, format!("保存失败: {e}")),
    }
}

/// 重置模糊音统计。
pub fn reset_fuzzy(settings: &mut Settings) {
    match call(&Request::ResetFuzzy) {
        Some(reply) => {
            settings.groups = reply.groups;
            settings.say(Notice::Success, "已重置模糊音组统计");
        }
        None => settings.say(Notice::Error, "输入法服务未运行,启动后再试"),
    }
}

/// 清空用户词库:要点两次,防误触。
pub fn clear_user(settings: &mut Settings) {
    settings.confirm_clear = false;
    if send(&Request::ClearUser) {
        settings.say(Notice::Success, "已清空用户词库");
    } else {
        settings.say(Notice::Error, "输入法服务未运行,启动后再试");
    }
}

/// 打开按应用配置文件。
pub fn open_apps(settings: &mut Settings) {
    settings.confirm_clear = false;
    open_text_file(
        settings,
        config::apps_path(),
        config::APPS_TEMPLATE,
        "没有可写的应用配置路径",
    );
}

/// 正在运行、有可见窗口且尚未配置的应用:exe 名 → 窗口标题。
pub(crate) fn running_apps(
    configured: &std::collections::BTreeMap<String, AppOverride>,
) -> Vec<(String, String)> {
    struct Found {
        configured: *const std::collections::BTreeMap<String, AppOverride>,
        apps: Vec<(String, String)>,
    }
    unsafe extern "system" fn visit(window: HWND, state: LPARAM) -> BOOL {
        let found = unsafe { &mut *(state.0 as *mut Found) };
        if !unsafe { IsWindowVisible(window) }.as_bool() {
            return BOOL(1);
        }
        let length = unsafe { GetWindowTextLengthW(window) };
        if length <= 0 {
            return BOOL(1);
        }
        let mut title = vec![0u16; length as usize + 1];
        let read = unsafe { GetWindowTextW(window, &mut title) };
        let title = String::from_utf16_lossy(&title[..read.max(0) as usize]);
        if title.trim().is_empty() {
            return BOOL(1);
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
        else {
            return BOOL(1);
        };
        let mut path = [0u16; 1024];
        let mut size = path.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(path.as_mut_ptr()),
                &mut size,
            )
        }
        .is_ok();
        let _ = unsafe { CloseHandle(process) };
        if !ok {
            return BOOL(1);
        }
        let image = String::from_utf16_lossy(&path[..size as usize]);
        let Some(exe) = std::path::Path::new(&image)
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
        else {
            return BOOL(1);
        };
        if exe == "explorer.exe" || exe == "lueying-settings.exe" {
            return BOOL(1);
        }
        if unsafe { &*found.configured }.contains_key(&exe)
            || found.apps.iter().any(|(known, _)| *known == exe)
        {
            return BOOL(1);
        }
        found.apps.push((exe, title));
        BOOL(1)
    }
    let mut found = Found {
        configured,
        apps: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut found as *mut Found as isize));
    }
    found
        .apps
        .sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    found.apps.truncate(40);
    found.apps
}

/// 打开一份用户数据文件;缺失时先写模板。保存后由 Server 在下次
/// 重载配置时读回。
fn open_text_file(settings: &mut Settings, path: Option<PathBuf>, template: &str, empty: &str) {
    let Some(path) = path else {
        settings.say(Notice::Error, empty);
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if !path.exists() {
        let _ = std::fs::write(&path, template);
    }
    match std::process::Command::new("notepad.exe").arg(&path).spawn() {
        Ok(_) => settings.say(Notice::Info, format!("已打开 {}", path.display())),
        Err(e) => settings.say(Notice::Error, format!("打开失败: {e}")),
    }
}

/// 打开场景目录:没有 scene.toml 就写一份官方示例当起点,再拉起资源
/// 管理器。用户改完存盘,候选窗一秒内热重载。
pub fn open_scene(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(dir) = config::scene_dir() else {
        settings.say(Notice::Error, "没有可写的场景目录");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        settings.say(Notice::Error, format!("建目录失败: {e}"));
        return;
    }
    let starter = dir.join("scene.toml");
    let mut written = false;
    if !starter.exists() {
        match std::fs::write(&starter, SCENE_STARTER) {
            Ok(()) => written = true,
            Err(e) => {
                settings.say(Notice::Error, format!("写 {} 失败: {e}", starter.display()));
                return;
            }
        }
    }
    match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
        Ok(_) => settings.say(
            Notice::Info,
            if written {
                format!("已写入示例场景,打开 {}", dir.display())
            } else {
                format!("已打开 {}", dir.display())
            },
        ),
        Err(e) => settings.say(Notice::Error, format!("打开失败: {e}")),
    }
}

/// 场景起步示例:打字光晕,官方示例库的第一个,改完存盘即生效。
const SCENE_STARTER: &str = r#"# 场景 = 候选窗的装饰层。改完存盘一秒内热重载;字段全表见 docs。
[signals.energy]
from = "typing_rate"
gain = 0.6
decay = "1.5s"

[[nodes]]
id = "glow"
kind = "shape"
shape = "rrect"
anchor = "highlight"
inset = -5
radius = 6
fill = "accent"
opacity = "lerp(0.15, 0.55, energy)"
effect = { kind = "blur", radius = "lerp(2, 10, energy)" }
"#;

/// PP-OCRv6 原生扩展是否齐全;设置页据此写「可用 / 未安装」。
pub fn ppocr_ready() -> bool {
    config::ppocr_dir().is_some()
}

/// 打开 PP-OCRv6 原生扩展目录:没有就建,并放一份文件名与下载说明。
pub fn open_ocr_models(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(dir) = config::extensions_dir().map(|root| root.join("ppocr")) else {
        settings.say(Notice::Error, "没有可写的扩展目录");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        settings.say(Notice::Error, format!("建目录失败: {e}"));
        return;
    }
    let readme = dir.join("README.txt");
    if !readme.exists() {
        let _ = std::fs::write(&readme, PPOCR_README);
    }
    match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
        Ok(_) => settings.say(Notice::Info, format!("已打开 {}", dir.display())),
        Err(e) => settings.say(Notice::Error, format!("打开失败: {e}")),
    }
}

const PPOCR_README: &str = "\
掠影 PP-OCRv6 识别引擎(原生扩展)目录

需要五个文件(推荐在「官方扩展」里一键安装,放好五件即视为已装):
  manifest.json  扩展清单
  ppocr.exe      识别引擎(独立进程,模型常驻)
  det.onnx       文字检测模型
  rec.onnx       文字识别模型
  dict.txt       词表,一行一个字

模型下载:
  https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx
  https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx
  dict.txt 从 rec 仓库的 inference.yml 里提取 character_dict,共 18708 行。

放好后在设置页把「截图识别」切到 PP-OCRv6,并在「运行期扩展」里同意;
首次识别会拉起引擎进程。
";

/// 打开运行期扩展目录:没有就建,再拉起资源管理器。每个包一个子
/// 目录,启用哪个由配置里的 `script` 决定。
pub fn open_scripts(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(dir) = config::extensions_dir() else {
        settings.say(Notice::Error, "没有可写的扩展目录");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        settings.say(Notice::Error, format!("建目录失败: {e}"));
        return;
    }
    match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
        Ok(_) => settings.say(Notice::Info, format!("已打开 {}", dir.display())),
        Err(e) => settings.say(Notice::Error, format!("打开失败: {e}")),
    }
}

/// 读扩展目录里的包清单与同意状态;坏包跳过,按显示名排序。设置页只读
/// 清单与哈希,不加载 Wasm,所以坏包也能先选上,由服务端记失败原因。
pub fn installed_scripts() -> Vec<ScriptEntry> {
    let Some(root) = config::extensions_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let book = config::extensions_consent_path()
        .as_deref()
        .map(ConsentBook::load)
        .unwrap_or_default();
    let mut scripts = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        let Ok(meta) = std::fs::metadata(&manifest_path) else {
            continue;
        };
        if meta.len() > lueying_extwasm::MAX_MANIFEST_BYTES as u64 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = lueying_extwasm::Manifest::parse(&bytes) else {
            continue;
        };
        if entry.file_name().to_str() != Some(manifest.id.as_str()) {
            continue;
        }
        let (consent, items) = match book.check(&path, &manifest) {
            Ok(check) => {
                let state = if check.approved {
                    ConsentState::Approved
                } else if check.changed {
                    ConsentState::Changed { added: check.added }
                } else {
                    ConsentState::Missing
                };
                (state, check.items)
            }
            Err(_) => (ConsentState::Missing, Vec::new()),
        };
        let native = manifest.is_native();
        scripts.push(ScriptEntry {
            id: manifest.id,
            name: manifest.name,
            native,
            items,
            consent,
            settings: manifest.settings,
        });
    }
    scripts.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    scripts
}

/// 同意配置里启用的扩展;单项列表与多项有序列表用同一条路径。
pub fn approve_extension(settings: &mut Settings) {
    settings.confirm_clear = false;
    let ids = settings.cfg.extensions.clone();
    if ids.is_empty() {
        settings.say(Notice::Error, "先在「运行期扩展」选择要启用的扩展");
        return;
    }
    let mut approved = 0usize;
    let mut failed = Vec::new();
    for id in &ids {
        match approve_package(id) {
            Ok(check) if check.required && !check.approved => approved += 1,
            Ok(_) => {}
            Err(error) => failed.push(format!("{id}: {error}")),
        }
    }
    settings.scripts = installed_scripts();
    if failed.is_empty() {
        settings.say(
            Notice::Success,
            format!("已同意 {approved} 个扩展(共启用 {} 个)", ids.len()),
        );
    } else {
        settings.say(Notice::Error, format!("同意失败:{}", failed.join(";")));
    }
}

/// 同意一个包:清单、哈希与能力写进记录;服务端下一拍重建运行期。
pub(crate) fn approve_package(id: &str) -> Result<ConsentCheck, String> {
    let root = config::extensions_dir().ok_or("没有扩展目录")?;
    let dir = root.join(id);
    let manifest = lueying_extwasm::manifest_at(&dir)?;
    let path = config::extensions_consent_path().ok_or("没有同意记录路径")?;
    let mut book = ConsentBook::load(&path);
    let check = book.check(&dir, &manifest)?;
    if check.required && !check.approved {
        book.approve(&dir, &manifest, now_ms())?;
        book.save(&path)?;
    }
    Ok(check)
}

/// 从文件导入一个 .luext 扩展包;文件对话框外壳,落盘在纯函数里。
pub fn import_luext(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(path) = choose_file(
        false,
        "",
        "导入扩展包",
        "掠影扩展包 (*.luext)\0*.luext\0所有文件 (*.*)\0*.*\0\0",
        "luext",
    ) else {
        return;
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            settings.say(
                Notice::Error,
                format!("读 {} 失败: {error}", path.display()),
            );
            return;
        }
    };
    if bytes.len() as u64 > MAX_PACKAGE_BYTES {
        settings.say(Notice::Error, "扩展包超过 128 MiB 上限");
        return;
    }
    let Some(root) = config::extensions_dir() else {
        settings.say(Notice::Error, "没有可写的扩展目录");
        return;
    };
    match install_luext_bytes(&bytes, &root) {
        Ok(message) => {
            settings.scripts = installed_scripts();
            settings.say(Notice::Success, message);
        }
        Err(error) => settings.say(Notice::Error, error),
    }
}

/// 单个 `.luext` 的读入上限;容器对单文件与全包另有校验。
const MAX_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;

/// 把 `.luext` 字节落盘到扩展目录;同 id 已有目录就整体替换。
///
/// 先解包到临时目录再改名换位,任何一步失败旧目录都保持原样。替换
/// 后包内容变了,同意记录对不上哈希,扩展自动挂起等用户重新确认。
fn install_luext_bytes(bytes: &[u8], root: &Path) -> Result<String, String> {
    let files = lueying_extmeta::container::decode(bytes)?;
    let Some((_, manifest_bytes)) = files.iter().find(|(name, _)| name == "manifest.json") else {
        return Err("包里没有 manifest.json".to_string());
    };
    let manifest = lueying_extmeta::Manifest::parse(manifest_bytes)
        .map_err(|e| format!("清单不合法: {e}"))?;
    let payload = manifest.payload_file().to_string();
    if !files.iter().any(|(name, _)| *name == payload) {
        return Err(format!("包里缺少 {payload}"));
    }
    let staging = root.join(format!(".import-{}", manifest.id));
    let target = root.join(&manifest.id);
    let backup = root.join(format!(".backup-{}", manifest.id));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("建临时目录失败: {e}"))?;
    for (name, content) in &files {
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 打开审计日志;没有就建一份空的。
pub fn open_ext_audit(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(path) = config::ext_audit_path() else {
        settings.say(Notice::Error, "没有可写的审计日志路径");
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if !path.exists() {
        let _ = std::fs::write(&path, "");
    }
    match std::process::Command::new("notepad.exe").arg(&path).spawn() {
        Ok(_) => settings.say(Notice::Info, format!("已打开 {}", path.display())),
        Err(e) => settings.say(Notice::Error, format!("打开失败: {e}")),
    }
}

/// 读服务端写下的运行指标;服务还没写就返回 `None`。
pub fn read_ext_metrics() -> Option<ExtMetrics> {
    let path = config::ext_metrics_path()?;
    path.exists().then(|| ExtMetrics::load(&path))
}

/// 刷新设置页的扩展运行摘要;数据在服务端每秒更新。
pub fn refresh_ext_metrics(settings: &mut Settings) {
    settings.confirm_clear = false;
    match read_ext_metrics() {
        Some(metrics) => {
            let count = metrics.extensions.len();
            settings.ext_metrics = Some(metrics);
            settings.say(Notice::Success, format!("已刷新 {count} 个扩展的运行数据"));
        }
        None => settings.say(Notice::Info, "服务还没有写入运行数据"),
    }
}

/// 拉一帧场景调试快照;只有类型与数值,正文内容永不出服务进程。
pub fn refresh_scene_debug(settings: &mut Settings) {
    settings.confirm_clear = false;
    let scene = call(&Request::SceneDebug).and_then(|reply| reply.scene);
    match scene {
        Some(scene) if scene.active => {
            settings.say(Notice::Success, scene_summary(&scene));
        }
        Some(_) => settings.say(Notice::Info, "场景层未启用;先把 scene.toml 放进场景目录"),
        None => settings.say(Notice::Error, "输入法服务未运行,启动后再试"),
    }
}

/// 快照摘要:最近 64 帧的 p50 与最大耗时,加事件总量与最近几个事件名。
fn scene_summary(scene: &lueying_protocol::SceneDebugReply) -> String {
    let mut frames = scene.frame_us.clone();
    frames.sort_unstable();
    let pick = |index: usize| {
        frames
            .get(index)
            .map(|us| format!("{:.1} ms", f64::from(*us) / 1000.0))
    };
    let frames_text = if frames.is_empty() {
        "暂无帧数据".to_string()
    } else {
        format!(
            "帧 p50 {} · 最大 {}",
            pick(frames.len() / 2).unwrap_or_default(),
            pick(frames.len() - 1).unwrap_or_default()
        )
    };
    let total: u32 = scene.counts.iter().map(|(_, count)| count).sum();
    let recent = scene
        .events
        .iter()
        .rev()
        .take(3)
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join("/");
    format!("{frames_text};事件 {total} 次,最近 {recent}")
}

/// 导出用户词库到文件。
pub fn export_user(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(path) = choose_user_file(true) else {
        return;
    };
    match call(&Request::ExportUser) {
        Some(reply) => match std::fs::write(&path, reply.user) {
            Ok(()) => settings.say(Notice::Success, format!("已导出到 {}", path.display())),
            Err(e) => settings.say(Notice::Error, format!("导出失败: {e}")),
        },
        None => settings.say(Notice::Error, "输入法服务未运行,启动后再试"),
    }
}

/// 从文件导入用户词库。
pub fn import_user(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(path) = choose_user_file(false) else {
        return;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            settings.say(Notice::Error, format!("读取失败: {e}"));
            return;
        }
    };
    match call(&Request::ImportUser { text }) {
        Some(reply) => {
            settings.groups = reply.groups;
            settings.say(Notice::Success, format!("已从 {} 合并", path.display()));
        }
        None => settings.say(Notice::Error, "输入法服务未运行,启动后再试"),
    }
}

/// 导出诊断报告:版本、配置、模糊组统计与服务日志末尾。
pub fn export_diagnostics(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(path) = choose_diag_file() else {
        return;
    };
    let log = config::log_path().and_then(|path| std::fs::read(path).ok());
    let (note, tail) = match &log {
        Some(bytes) => ("", tail_text(bytes, 64 * 1024)),
        None => ("(没有找到服务日志)\n", String::new()),
    };
    let groups = settings
        .groups
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>();
    let status = call(&Request::Status).and_then(|reply| reply.status);
    let server_version = status
        .as_ref()
        .map(|s| s.server_version.clone())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let report = format!(
        "掠影诊断报告\n\
         生成时间: {}\n\
         设置窗版本: {}\n\
         服务版本: {}\n\
         资源: {}\n\
         系统: {} / {}\n\
         模糊组: {}\n\
         配置: {:?}\n\n\
         --- 服务日志(末尾 64 KB)---\n\
         {note}{tail}\n",
        local_stamp(),
        settings.current,
        server_version,
        status_detail(&status),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if groups.is_empty() {
            "(无)".to_string()
        } else {
            groups.join(", ")
        },
        settings.cfg,
    );
    match std::fs::write(&path, report) {
        Ok(()) => settings.say(Notice::Success, format!("已导出到 {}", path.display())),
        Err(e) => settings.say(Notice::Error, format!("导出失败: {e}")),
    }
}

/// 更新确认框;确认后由设置窗后台下载并运行安装程序。
pub fn install_update(settings: &mut Settings, release: &update::Release) -> bool {
    settings.confirm_clear = false;
    let text = format!(
        "安装掠影 {}？\n\n安装包会下载到临时目录,校验后直接运行安装程序。",
        release.version
    );
    let confirmed = unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("检查更新"),
            MB_OKCANCEL | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) == IDOK
    };
    confirmed
}

/// 后台下载安装包到临时目录,校验后直接运行安装程序;由设置窗线程调。
pub fn download_and_run_installer(release: &update::Release) -> Result<(), String> {
    let path = update::download_installer(release)?;
    Command::new(&path)
        .spawn()
        .map_err(|e| format!("启动安装程序: {e}"))?;
    Ok(())
}

/// 系统文件对话框;取消或失败时返回 None。
fn choose_user_file(save: bool) -> Option<PathBuf> {
    choose_file(
        save,
        if save { "lueying-user.tsv" } else { "" },
        if save {
            "导出用户词库"
        } else {
            "导入用户词库"
        },
        "用户词库 (*.tsv)\0*.tsv\0所有文件 (*.*)\0*.*\0\0",
        "tsv",
    )
}

/// 诊断报告的另存对话框。
fn choose_diag_file() -> Option<PathBuf> {
    choose_file(
        true,
        "lueying-diag.txt",
        "导出诊断",
        "文本文件 (*.txt)\0*.txt\0所有文件 (*.*)\0*.*\0\0",
        "txt",
    )
}

/// `filter` 自带双 NUL 结尾;标题与扩展名由这里补结尾 NUL。
fn choose_file(
    save: bool,
    name: &str,
    title: &str,
    filter: &str,
    extension: &str,
) -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    for (slot, ch) in buf.iter_mut().zip(name.encode_utf16()) {
        *slot = ch;
    }
    let filter: Vec<u16> = filter.encode_utf16().collect();
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let extension: Vec<u16> = extension.encode_utf16().chain(std::iter::once(0)).collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_PATHMUSTEXIST
            | if save {
                OFN_OVERWRITEPROMPT
            } else {
                OFN_FILEMUSTEXIST
            },
        lpstrDefExt: PCWSTR(extension.as_ptr()),
        ..Default::default()
    };
    let picked = unsafe {
        if save {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        }
    };
    if !picked.as_bool() {
        return None;
    }
    let len = buf.iter().position(|&ch| ch == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// 诊断报告抬头的本地时间。
fn local_stamp() -> String {
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// 日志只取末尾一段;截断处可能切在字符中间,按替换字符解码。
fn tail_text(bytes: &[u8], limit: usize) -> String {
    let start = bytes.len().saturating_sub(limit);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
#[path = "settings_actions_tests.rs"]
mod tests;
