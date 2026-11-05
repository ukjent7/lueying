//! 配置备份的导出、导入与块解析。
//!
//! `settings::actions` 的子模块;文件对话框
//! 与时间戳助手经 `super::*` 取用。

use super::*;
use std::io::Write;

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
        ("config.toml", config::path()),
        ("phrases.txt", config::phrases_path()),
        ("punct.txt", config::punct_path()),
        ("apps.toml", config::apps_path()),
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
    let out = crate::sync::pack(&files);
    // 临时文件 + fsync + 改名:失败与断电都不留半截备份。
    let tmp = path.with_extension("bak.tmp");
    let written = std::fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(out.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, &path));
    match written {
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
    if !crate::sync::is_bundle(&text) {
        settings.say(Notice::Error, "这份文件不是掠影备份");
        return;
    }
    let files = crate::sync::unpack(&text);
    let mut written = 0usize;
    let mut imported_user = false;
    for (name, content) in &files {
        match name.as_str() {
            "user.tsv" => {
                if send(&Request::ImportUser {
                    text: content.clone(),
                }) {
                    imported_user = true;
                }
                continue;
            }
            "config.toml" | "phrases.txt" | "punct.txt" | "apps.toml" => {}
            _ => continue,
        }
        let target = match name.as_str() {
            "config.toml" => config::path(),
            "phrases.txt" => config::phrases_path(),
            "punct.txt" => config::punct_path(),
            "apps.toml" => config::apps_path(),
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
