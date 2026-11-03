//! 子进程句柄:普通启动复用 std [`std::process::Child`],Windows 沙箱
//! 直接持有 `CreateProcessW` 的原始句柄。std 的 `Child` 不能从原始句柄
//! 构造,这一层把两条来路收成同一组操作。

use std::io::{Read, Write};
use std::process::ExitStatus;

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;
#[cfg(windows)]
use windows::Win32::Foundation::{HANDLE, STILL_ACTIVE};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetExitCodeProcess, TerminateProcess};

enum Stdin {
    #[cfg(windows)]
    File(std::fs::File),
    Std(std::process::ChildStdin),
}

impl Write for Stdin {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::File(file) => file.write(buf),
            Self::Std(stdin) => stdin.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::File(file) => file.flush(),
            Self::Std(stdin) => stdin.flush(),
        }
    }
}

enum Stdout {
    #[cfg(windows)]
    File(std::fs::File),
    Std(std::process::ChildStdout),
}

impl Read for Stdout {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::File(file) => file.read(buf),
            Self::Std(stdout) => stdout.read(buf),
        }
    }
}

enum Inner {
    #[cfg(windows)]
    Raw(OwnedHandle),
    Std(std::process::Child),
}

/// 原生扩展子进程;标准输入输出在构造时取走。
pub struct Process {
    inner: Inner,
    stdin: Option<Stdin>,
    stdout: Option<Stdout>,
}

impl Process {
    /// 接管 `CreateProcessW` 返回的进程句柄与父侧管道端。
    ///
    /// # Safety
    ///
    /// 三个句柄都必须有效且由本对象独占所有权。
    #[cfg(windows)]
    pub(crate) unsafe fn from_raw(process: HANDLE, stdin: HANDLE, stdout: HANDLE) -> Self {
        Self {
            inner: Inner::Raw(unsafe { OwnedHandle::from_raw_handle(process.0) }),
            stdin: Some(Stdin::File(unsafe {
                std::fs::File::from_raw_handle(stdin.0)
            })),
            stdout: Some(Stdout::File(unsafe {
                std::fs::File::from_raw_handle(stdout.0)
            })),
        }
    }

    /// 取走标准输入写端。
    pub fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        self.stdin
            .take()
            .map(|stdin| Box::new(stdin) as Box<dyn Write + Send>)
    }

    /// 取走标准输出读端。
    pub fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stdout
            .take()
            .map(|stdout| Box::new(stdout) as Box<dyn Read + Send>)
    }

    /// 非阻塞探测进程是否退出;返回 `Ok(None)` 表示仍在运行。
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match &mut self.inner {
            #[cfg(windows)]
            Inner::Raw(handle) => unsafe {
                let mut code = 0u32;
                GetExitCodeProcess(HANDLE(handle.as_raw_handle()), &mut code)
                    .map_err(std::io::Error::other)?;
                if code == STILL_ACTIVE.0 as u32 {
                    Ok(None)
                } else {
                    Ok(Some(ExitStatus::from_raw(code)))
                }
            },
            Inner::Std(child) => child.try_wait(),
        }
    }

    /// 阻塞等待进程退出。
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match &mut self.inner {
            #[cfg(windows)]
            Inner::Raw(_) => loop {
                if let Some(status) = self.try_wait()? {
                    return Ok(status);
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            },
            Inner::Std(child) => child.wait(),
        }
    }

    /// 终止进程。
    pub fn kill(&mut self) -> std::io::Result<()> {
        match &mut self.inner {
            #[cfg(windows)]
            Inner::Raw(handle) => unsafe {
                TerminateProcess(HANDLE(handle.as_raw_handle()), 1).map_err(std::io::Error::other)
            },
            Inner::Std(child) => child.kill(),
        }
    }

    /// 进程句柄,Job Object 挂接用。
    #[cfg(windows)]
    pub(crate) fn raw_handle(&self) -> HANDLE {
        match &self.inner {
            Inner::Raw(handle) => HANDLE(handle.as_raw_handle()),
            Inner::Std(child) => HANDLE(child.as_raw_handle()),
        }
    }
}

impl From<std::process::Child> for Process {
    fn from(mut child: std::process::Child) -> Self {
        let stdin = child.stdin.take().map(Stdin::Std);
        let stdout = child.stdout.take().map(Stdout::Std);
        Self {
            inner: Inner::Std(child),
            stdin,
            stdout,
        }
    }
}
