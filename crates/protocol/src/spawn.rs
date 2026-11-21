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
    use windows::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{PCWSTR, w};

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let exe_wide = wide(exe);
    let mut params = String::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            params.push(' ');
        }
        params.push_str(arg);
    }
    let params_wide: Vec<u16> = params.encode_utf16().chain(std::iter::once(0)).collect();
    let dir_wide = dir.map(wide);
    // uiAccess 程序只认 ShellExecute 的拉起路径:普通过滤令牌 CreateProcessW
    // 会被系统以 740(需要提升)拒掉,ShellExecuteEx 走 Shell 代行不触发
    // 该校验——输入法宿主与安装辅助程序都是普通过滤令牌,必须走这条路。
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("open"),
        lpFile: PCWSTR(exe_wide.as_ptr()),
        lpParameters: PCWSTR(if params.is_empty() {
            std::ptr::null()
        } else {
            params_wide.as_ptr()
        }),
        lpDirectory: PCWSTR(dir_wide.as_ref().map_or(std::ptr::null(), |d| d.as_ptr())),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|error| format!("启动 {}: {error}", exe.display()))?;
        if !info.hProcess.is_invalid() {
            let _ = CloseHandle(info.hProcess);
        }
    }
    Ok(())
}
