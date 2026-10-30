//! 扩展能力调用审计:谁在什么时候调了哪个宿主服务。
//!
//! 只记调用方、服务、方法与结果,不记参数里的用户内容。日志按 1 MiB
//! 滚动一份,和 `server.log` 同目录;设置页有入口直接打开。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use lueying_extjs::{AuditOutcome, AuditSink};
use windows::Win32::System::SystemInformation::GetLocalTime;

const LIMIT: u64 = 1024 * 1024;

/// 写文件的审计接收端;文件打不开时退回服务日志一条说明。
pub struct FileAudit {
    path: PathBuf,
    file: Mutex<Option<std::fs::File>>,
}

impl FileAudit {
    /// 打开审计文件;超过上限先滚动。失败不阻止扩展运行。
    pub fn open(path: PathBuf) -> Self {
        if let Some(dir) = path.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            println!("扩展审计目录不可用: {error}");
        }
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LIMIT) {
            let old = path.with_file_name("ext-audit.log.1");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&path, &old);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| {
                println!("扩展审计文件不可用: {error}");
                error
            })
            .ok();
        Self {
            path,
            file: Mutex::new(file),
        }
    }

    /// 日志里展示的路径;设置页的「查看」用。
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl AuditSink for FileAudit {
    fn record(&self, caller: &str, service: &str, method: &str, outcome: AuditOutcome, note: &str) {
        let result = match outcome {
            AuditOutcome::Allowed => "允许",
            AuditOutcome::Denied => "拒绝",
        };
        let note = note.replace(['\r', '\n'], " ");
        let line = format!(
            "{} {caller} {service}.{method} {result} {note}\n",
            local_stamp()
        );
        let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(file) = file.as_mut()
            && let Err(error) = file.write_all(line.as_bytes())
        {
            println!("扩展审计写入失败: {error}");
        }
    }
}

fn local_stamp() -> String {
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}
