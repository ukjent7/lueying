//! 设置窗的平台侧动作:管道、文件对话框、备份与自动更新。
//!
//! 页面与配置改动在 [`crate::settings_view`];这里只做「要动文件系统、
//! 要问常驻服务」的事,以及那些离不开 Windows API 的对话框。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use crate::config;
use crate::settings_view::{Notice, ScriptEntry, Settings, status_detail};
use crate::update;
use lueying_protocol::{Reply, Request, pipe_name};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, GetSaveFileNameW, OFN_FILEMUSTEXIST, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IDOK, MB_DEFBUTTON2, MB_ICONQUESTION,
    MB_OKCANCEL, MF_CHECKED, MF_SEPARATOR, MF_STRING, MessageBoxW, SetForegroundWindow,
    TPM_NONOTIFY, TPM_RETURNCMD, TrackPopupMenu,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// 给常驻 Server 发一条请求并取回应答;没在运行返回 None。
pub fn call(request: &Request) -> Option<Reply> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name())
        .ok()?;
    let mut line = serde_json::to_string(request).ok()?;
    line.push('\n');
    file.write_all(line.as_bytes()).ok()?;
    file.flush().ok()?;
    let mut reply = String::new();
    BufReader::new(file).read_line(&mut reply).ok()?;
    serde_json::from_str(&reply).ok()
}

/// 给常驻 Server 发一条请求;没在运行返回 false。
pub fn send(request: &Request) -> bool {
    call(request).is_some()
}

/// 通知常驻 Server 重读配置。
pub fn reload() {
    let _ = send(&Request::ReloadConfig);
}

/// 存盘:写配置并让常驻服务重读。
pub fn save(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(path) = settings.path.clone() else {
        settings.say(Notice::Error, "没有可写的配置路径");
        return;
    };
    match config::save(&path, &settings.cfg) {
        Ok(()) => {
            reload();
            settings.say(Notice::Success, "已保存");
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

/// 打开自定义短语文件。
pub fn open_phrases(settings: &mut Settings) {
    settings.confirm_clear = false;
    open_text_file(
        settings,
        config::phrases_path(),
        lueying_features::phrases::TEMPLATE,
        "没有可写的短语文件路径",
    );
}

/// 打开自定义标点文件。
pub fn open_punct(settings: &mut Settings) {
    settings.confirm_clear = false;
    open_text_file(
        settings,
        config::punct_path(),
        config::PUNCT_TEMPLATE,
        "没有可写的标点文件路径",
    );
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

/// 打开用户效果目录:先让常驻服务给出三个槽位的模板(缺文件才写),
/// 再拉起资源管理器。模板是起点,用户改完存盘即生效。
pub fn open_effects(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(dir) = config::effects_dir() else {
        settings.say(Notice::Error, "没有可写的效果目录");
        return;
    };
    let Some(reply) = call(&Request::EffectTemplates) else {
        settings.say(Notice::Error, "输入法服务未运行,启动后再试");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        settings.say(Notice::Error, format!("建目录失败: {e}"));
        return;
    }
    let mut written = 0;
    for (name, source) in &reply.templates {
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        match std::fs::write(&path, source) {
            Ok(()) => written += 1,
            Err(e) => {
                settings.say(Notice::Error, format!("写 {} 失败: {e}", path.display()));
                return;
            }
        }
    }
    match std::process::Command::new("explorer.exe").arg(&dir).spawn() {
        Ok(_) => settings.say(
            Notice::Info,
            if written > 0 {
                format!("已写入 {written} 个模板,打开 {}", dir.display())
            } else {
                format!("已打开 {}", dir.display())
            },
        ),
        Err(e) => settings.say(Notice::Error, format!("打开失败: {e}")),
    }
}

/// 打开运行期脚本目录:没有就建,再拉起资源管理器。每个包一个子
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

/// 读扩展目录里的包清单;坏包跳过,按显示名排序。设置页只读清单,
/// 不加载 JS,所以坏脚本也能先选上,由服务端记失败原因。
pub fn installed_scripts() -> Vec<ScriptEntry> {
    let Some(root) = config::extensions_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
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
        if meta.len() > lueying_extjs::MAX_MANIFEST_BYTES as u64 {
            continue;
        }
        let Ok(bytes) = std::fs::read(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = lueying_extjs::Manifest::parse(&bytes) else {
            continue;
        };
        if manifest.script_main().is_none() {
            continue;
        }
        if entry.file_name().to_str() != Some(manifest.id.as_str()) {
            continue;
        }
        scripts.push(ScriptEntry {
            id: manifest.id,
            name: manifest.name,
        });
    }
    scripts.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    scripts
}

/// 弹出扩展选择菜单:关闭,或启用某个已安装的包。选择只改配置,
/// 点保存后服务端才重载。多扩展的有序列表在配置文件里维护;这个
/// 菜单保持单选,选中一个就替换列表。
pub fn choose_script(hwnd: HWND, settings: &mut Settings) {
    settings.confirm_clear = false;
    settings.scripts = installed_scripts();
    if settings.scripts.is_empty() {
        settings.say(Notice::Info, "没有已安装的扩展;点「目录」放入扩展包");
        return;
    }
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        settings.say(Notice::Error, "建菜单失败");
        return;
    };
    let selected = settings.script_selected();
    let off = HSTRING::from("关闭运行期脚本");
    unsafe {
        let flags = if settings.cfg.extensions.is_empty() {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = AppendMenuW(menu, flags, 1, &off);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        for (index, script) in settings.scripts.iter().enumerate() {
            let label = HSTRING::from(format!("{}  {}", escape_menu(&script.name), script.id));
            let flags = if selected == Some(index) {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(menu, flags, 2 + index, &label);
        }
    }
    let mut point = POINT { x: 0, y: 0 };
    let _ = unsafe { GetCursorPos(&mut point) };
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
    let command = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        )
    };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    match command.0 as usize {
        0 => {}
        1 => {
            settings.select_script(None);
            settings.say(Notice::Info, "已关闭运行期脚本,保存后生效");
        }
        command => {
            let index = command - 2;
            if let Some(script) = settings.scripts.get(index) {
                let name = script.name.clone();
                settings.select_script(Some(index));
                settings.say(Notice::Info, format!("已选择 {name},保存后生效"));
            }
        }
    }
}

/// 菜单里的 `&` 是助记符;包名里的 `&` 要写成 `&&`。
fn escape_menu(text: &str) -> String {
    text.replace('&', "&&")
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

/// 导出配置备份:配置、短语、标点、按应用配置与用户词库打包成
/// 一份文本。文件内容按行数声明长度,文件里出现什么标记都不影响
/// 解析;服务未运行时词库一块跳过。
pub fn backup_export(settings: &mut Settings) {
    settings.confirm_clear = false;
    let stamp = local_stamp().replace([':', ' '], "-");
    let Some(path) = choose_file(
        true,
        &format!("lueying-backup-{stamp}.txt"),
        "导出配置备份",
        "备份文件 (*.txt)\0*.txt\0所有文件 (*.*)\0*.*\0\0",
        "txt",
    ) else {
        return;
    };
    let mut files: Vec<(String, String)> = Vec::new();
    for (name, path) in [
        ("config.ini", config::path()),
        ("phrases.txt", config::phrases_path()),
        ("punct.txt", config::punct_path()),
        ("apps.ini", config::apps_path()),
    ] {
        if let Some(path) = path
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            files.push((name.to_string(), text));
        }
    }
    if let Some(reply) = call(&Request::ExportUser) {
        files.push(("user.tsv".to_string(), reply.user));
    }
    if files.is_empty() {
        settings.say(Notice::Error, "没有可备份的数据");
        return;
    }
    let mut out = String::from("### lueying-backup v1\n");
    for (name, content) in &files {
        let lines = content.lines().count();
        out.push_str(&format!("### begin: {name} {lines}\n"));
        if lines > 0 {
            out.push_str(content.trim_end_matches('\n'));
            out.push('\n');
        }
        out.push_str(&format!("### end: {name}\n"));
    }
    match std::fs::write(&path, out) {
        Ok(()) => settings.say(Notice::Success, format!("已备份到 {}", path.display())),
        Err(e) => settings.say(Notice::Error, format!("导出失败: {e}")),
    }
}

/// 导入配置备份:逐块写回对应文件,用户词库走管道合并;最后让
/// Server 重读配置并刷新设置窗里的当前值。
pub fn backup_import(settings: &mut Settings) {
    settings.confirm_clear = false;
    let Some(path) = choose_file(
        false,
        "",
        "导入配置备份",
        "备份文件 (*.txt)\0*.txt\0所有文件 (*.*)\0*.*\0\0",
        "txt",
    ) else {
        return;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => {
            settings.say(Notice::Error, format!("读取失败: {e}"));
            return;
        }
    };
    if !text.starts_with("### lueying-backup v1") {
        settings.say(Notice::Error, "这份文件不是掠影备份");
        return;
    }
    let files = unpack_backup(&text);
    let mut written = 0usize;
    let mut imported_user = false;
    for (name, content) in &files {
        let target = match name.as_str() {
            "config.ini" => config::path(),
            "phrases.txt" => config::phrases_path(),
            "punct.txt" => config::punct_path(),
            "apps.ini" => config::apps_path(),
            "user.tsv" => {
                if send(&Request::ImportUser {
                    text: content.clone(),
                }) {
                    imported_user = true;
                }
                continue;
            }
            _ => continue,
        };
        if let Some(path) = target {
            let body = if content.is_empty() {
                String::new()
            } else {
                format!("{content}\n")
            };
            if std::fs::write(&path, body).is_ok() {
                written += 1;
            }
        }
    }
    reload();
    if let Some(path) = settings.path.clone() {
        settings.cfg = config::load(&path);
    }
    if written > 0 || imported_user {
        settings.say(
            Notice::Success,
            format!(
                "已恢复 {written} 份配置文件{}",
                if imported_user { "与词库" } else { "" }
            ),
        );
    } else {
        settings.say(Notice::Error, "备份里没有可恢复的内容");
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

/// 确认后把更新器复制到临时目录并启动;设置窗退出,更新器接管安装。
pub fn install_update(settings: &mut Settings, release: &update::Release) -> bool {
    settings.confirm_clear = false;
    let text = format!(
        "安装掠影 {}？\n\n设置窗口会关闭,输入法服务短暂重启。",
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
    if !confirmed {
        return false;
    }
    match update::launch_updater(release, std::process::id()) {
        Ok(()) => true,
        Err(error) => {
            settings.say(Notice::Error, format!("启动更新失败: {error}"));
            false
        }
    }
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

/// 解析备份文件:`### begin: <名字> <行数>` 之后逐行取声明数量的行,
/// 随后跳过 `### end:` 行;坏块整块跳过。
fn unpack_backup(text: &str) -> Vec<(String, String)> {
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
