//! 命名管道监听:多实例阻塞模式,每连接一个线程,行 JSON 请求-应答。
//! 建法与青简 server 相同:CreateNamedPipeW + ConnectNamedPipe,句柄交给 `File` 管。

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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

use lueying_protocol::Request;

use crate::imp::State;

const BUFFER_SIZE: u32 = 64 * 1024;
/// 一条请求行的上限。DLL 的一行远小于这个数;不发换行的坏对端不能把
/// 内存撑爆,超限直接断线。
const MAX_LINE: u64 = 1024 * 1024;
/// 连接号只增不减;用于区分同一会话的重连与旧连接断开。
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

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
    let mut line = Vec::new();
    let conn = NEXT_CONN.fetch_add(1, Ordering::Relaxed);
    let mut session = None;
    loop {
        line.clear();
        // 单行上限:对端不发换行也不能把内存撑爆;`take` 读满仍没有换行,
        // 剩下的半行不能当成下一条请求,直接断线。
        match reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if !line.ends_with(b"\n") => break,
            Ok(_) => {}
        }
        let request = std::str::from_utf8(&line)
            .ok()
            .and_then(|text| serde_json::from_str::<Request>(text.trim()).ok());
        // Hello 与连接绑定:同一会话重连后,旧连接断开不能删掉新会话。
        if let Some(Request::Hello {
            session: hello_session,
            ..
        }) = request.as_ref()
        {
            state
                .conns
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(*hello_session, conn);
            session = Some(*hello_session);
        }
        // 正常下线立即解除连接绑定;旧连接的断开回调不能再清理已重连的会话。
        if let Some(Request::Goodbye {
            session: gone_session,
        }) = request.as_ref()
        {
            let mut conns = state.conns.lock().unwrap_or_else(|p| p.into_inner());
            if conns.get(gone_session) == Some(&conn) {
                conns.remove(gone_session);
            }
        }
        let reply = request
            .map(|req| {
                // 单个坏请求不能带崩常驻服务;回一个不改文档、不改模式的中性应答。
                catch_unwind(AssertUnwindSafe(|| crate::imp::handle(&state, req))).unwrap_or_else(
                    |payload| {
                        // 服务日志已接到文件,panic 内容要落盘,否则只剩一句「出错了」。
                        let msg = payload
                            .downcast_ref::<&str>()
                            .copied()
                            .or(payload.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("<非字符串 panic>");
                        eprintln!("请求处理 panic,已回中性应答:{msg}");
                        crate::imp::neutral(&state)
                    },
                )
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
    // 宿主崩溃/被结束不会发 Goodbye;连接断开即清理会话,词典与重排缓存不常驻。
    if let Some(session) = session {
        let mut conns = state.conns.lock().unwrap_or_else(|p| p.into_inner());
        if conns.get(&session) == Some(&conn) {
            conns.remove(&session);
            drop(conns);
            println!("会话 {session} 连接断开,清理");
            let _ = crate::imp::handle(&state, Request::Goodbye { session });
        }
    }
}
