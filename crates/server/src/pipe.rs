//! 命名管道监听:多实例阻塞模式,每连接一个线程,行 JSON 请求-应答。
//! 建法与青简 server 相同:CreateNamedPipeW + ConnectNamedPipe,句柄交给 `File` 管。

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::core::{HRESULT, HSTRING};

use pinyin_protocol::Request;

use crate::imp::State;

const BUFFER_SIZE: u32 = 64 * 1024;

/// 阻塞主线程:循环建管道实例并等待连接。永不返回。
pub(super) fn listen(name: &str, state: Arc<State>) -> ! {
    let wide: HSTRING = name.into();
    loop {
        match create_instance(&wide).and_then(wait_client) {
            Ok(stream) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || serve(stream, state));
            }
            Err(e) => {
                eprintln!("管道实例建立失败: {e},5 秒后重试");
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
    }
}

/// 建一个实例。句柄交给 `File` 管:对端关闭时 std 把 `ERROR_BROKEN_PIPE` 当 EOF。
fn create_instance(name: &HSTRING) -> io::Result<File> {
    // 系统应用(UWP/设置/搜索)以低完整性、AppContainer 令牌连管道;默认 DACL
    // 只给当前用户,它们会连不上而变成打不出中文。放行对象所有者(当前用户)、
    // AppContainer 与受限 AppContainer,低完整性标签让低 IL 客户端也能读写;
    // 管道名本身仍带每用户后缀。
    let security = security_attributes();
    let handle = unsafe {
        CreateNamedPipeW(
            name,
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            security
                .as_ref()
                .map(|(attributes, _)| attributes as *const SECURITY_ATTRIBUTES),
        )
    };
    if let Some((_, descriptor)) = security {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        }
    }
    if handle.is_invalid() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

/// `SECURITY_ATTRIBUTES` 与它指向的描述符要一起返回,调用方建完管道再释放。
fn security_attributes() -> Option<(SECURITY_ATTRIBUTES, PSECURITY_DESCRIPTOR)> {
    let sddl = HSTRING::from("D:(A;;GA;;;OW)(A;;GA;;;AC)(A;;GA;;;S-1-15-2-2)S:(ML;;NW;;;LW)");
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &sddl,
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .ok()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    Some((attributes, descriptor))
}

/// 阻塞等一个客户端连上这个实例。
fn wait_client(stream: File) -> io::Result<File> {
    let handle = HANDLE(stream.as_raw_handle());
    // 客户端在建实例与 ConnectNamedPipe 之间就连上了也算成功。
    if let Err(error) = unsafe { ConnectNamedPipe(handle, None) }
        && error.code() != HRESULT::from_win32(ERROR_PIPE_CONNECTED.0)
    {
        return Err(io::Error::other(error));
    }
    Ok(stream)
}

fn serve(file: File, state: Arc<State>) {
    // 记录客户端 PID:键盘数据的来源进程,日志排查与后续限权用(mozc 同款)。
    let mut pid = 0u32;
    if unsafe { GetNamedPipeClientProcessId(HANDLE(file.as_raw_handle()), &mut pid) }.is_ok() {
        println!("管道客户端 pid={pid}");
    }
    let mut writer = match file.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let reply = serde_json::from_str::<Request>(line.trim())
            .ok()
            .map(|req| {
                // 单个坏请求不能带崩常驻服务;回一个不改文档、不改模式的中性应答。
                catch_unwind(AssertUnwindSafe(|| crate::imp::handle(&state, req)))
                    .unwrap_or_else(|_| {
                        eprintln!("请求处理 panic,已回中性应答");
                        crate::imp::neutral(&state)
                    })
            })
            .unwrap_or_else(|| crate::imp::neutral(&state));
        let mut out = match serde_json::to_string(&reply) {
            Ok(s) => s,
            Err(_) => break,
        };
        out.push('\n');
        if writer.write_all(out.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }
}
