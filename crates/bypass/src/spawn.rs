//! 原生扩展进程的拉起:平台分派。
//!
//! Windows 走 [`super::sandbox`]:无能力的 AppContainer、
//! CREATE_SUSPENDED 先挂限额再恢复;其它平台直接 `std::process::Command`。

use std::path::{Path, PathBuf};
use std::process::Child;

#[cfg(not(windows))]
use std::process::{Command, Stdio};

use super::ProcessLimiter;

/// 拉起原生扩展进程;返回时进程已挂好限额。`readable` 是要授予
/// AppContainer 只读执行权限的额外路径,Windows 之外的平台忽略。
pub(crate) fn spawn_ext(
    exe: &Path,
    args: &[String],
    readable: &[PathBuf],
    limiter: Option<&dyn ProcessLimiter>,
) -> Result<Child, String> {
    #[cfg(windows)]
    {
        super::sandbox::spawn_suspended(exe, args, readable, limiter)
    }
    #[cfg(not(windows))]
    {
        let _ = readable;
        let mut command = Command::new(exe);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|error| format!("原生扩展启动失败: {error}"))?;
        if let Some(limiter) = limiter
            && let Err(error) = limiter.attach(&child)
        {
            crate::kill(&mut child);
            return Err(error);
        }
        Ok(child)
    }
}
