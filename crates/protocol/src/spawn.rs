//! 拉起常驻服务进程。
//!
//! Windows 的 `CreateProcessW` 默认把调用方所有可继承句柄复制给子进程,
//! `std::process::Command` 也是这个默认值;把 stdio 设成 null 只替换三个
//! 标准句柄,不会关闭对其他可继承句柄的继承。服务常驻后若持有调用方
//! stdout 管道的写端,调用方读取输出就永远等不到 EOF(安装器/更新器在
//! CI 里就是这么挂住的)。这里直接调 `CreateProcessW` 并关闭句柄继承。

use std::path::Path;

/// 起一个不继承任何句柄的独立进程。服务不需要 stdin/stdout/stderr。
#[cfg(windows)]
pub fn spawn_detached(exe: &Path, dir: Option<&Path>) -> Result<(), String> {
    spawn_detached_args(exe, &[], dir)
}

/// 同 [`spawn_detached`],但带命令行参数(如设置窗的 `models fetch`)。
/// 参数按空格拼接,只用于不含空格的固定子命令。
#[cfg(windows)]
pub fn spawn_detached_args(exe: &Path, args: &[&str], dir: Option<&Path>) -> Result<(), String> {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
    };
    use windows::core::{PCWSTR, PWSTR};

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let exe_wide = wide(exe);
    let mut line = format!("\"{}\"", exe.display());
    for arg in args {
        line.push(' ');
        line.push_str(arg);
    }
    // CreateProcessW 会就地改写命令行,必须给可写缓冲。
    let mut command: Vec<u16> = line.encode_utf16().chain(std::iter::once(0)).collect();
    let dir_wide = dir.map(wide);
    let dir_ptr = PCWSTR(
        dir_wide
            .as_ref()
            .map_or(std::ptr::null(), |dir| dir.as_ptr()),
    );
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(exe_wide.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            dir_ptr,
            &startup,
            &mut process,
        )
        .map_err(|error| format!("启动 {}: {error}", exe.display()))?;
        let _ = CloseHandle(process.hProcess);
        let _ = CloseHandle(process.hThread);
    }
    Ok(())
}
