//! 原生扩展进程的拉起:平台分派。
//!
//! Windows 沙箱走 [`super::sandbox`]:无能力的 AppContainer、
//! CREATE_SUSPENDED 先挂限额再恢复;不套沙箱与其它平台直接
//! `std::process::Command`。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{Process, ProcessLimiter};

/// 拉起原生扩展进程;返回时进程已挂好限额。`readable` 是要授予
/// AppContainer 只读执行权限的额外路径,非沙箱与其它平台忽略;
/// `sandboxed` 关掉时用普通进程拉起(自有组件要联网时用)。
pub(crate) fn spawn_ext(
    exe: &Path,
    args: &[String],
    readable: &[PathBuf],
    limiter: Option<&dyn ProcessLimiter>,
    sandboxed: bool,
) -> Result<Process, String> {
    #[cfg(windows)]
    if sandboxed {
        return super::sandbox::spawn_suspended(exe, args, readable, limiter);
    }
    // 沙箱分支只在 Windows 编译;其它平台这两个参数没有去处。
    #[cfg(not(windows))]
    let _ = sandboxed;
    let _ = readable;
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = command
        .spawn()
        .map_err(|error| format!("原生扩展启动失败: {error}"))?;
    let mut child = Process::from(child);
    if let Some(limiter) = limiter
        && let Err(error) = limiter.attach(&child)
    {
        super::kill(&mut child);
        return Err(error);
    }
    Ok(child)
}
