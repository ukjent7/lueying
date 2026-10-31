//! 命名管道监听:多实例阻塞模式,每连接一个线程,行 JSON 请求-应答。
//! 建法与青简 server 相同:CreateNamedPipeW + ConnectNamedPipe,句柄交给 `File` 管。

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, OpenProcessToken, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TokenIsAppContainer,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{HRESULT, HSTRING, PWSTR};

use lueying_protocol::{MANAGE_CLIENTS, Privilege, Request};

use crate::imp::State;

const BUFFER_SIZE: u32 = 64 * 1024;
/// 一条请求行的上限。DLL 的一行远小于这个数;不发换行的坏对端不能把
/// 内存撑爆,超限直接断线。
const MAX_LINE: u64 = 1024 * 1024;
/// 处理耗时超过这个值的请求记进服务日志。键入路径的常态是 10 ms 上下;
/// 超过通常是 4M/25M 推理或词典冷页,排障时拿它对齐宿主卡顿。
const SLOW_REQUEST_MS: u128 = 100;
/// 连接号只增不减;用于区分同一会话的重连与旧连接断开。
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// 阻塞主线程:循环建管道实例并等待连接。永不返回。
pub(super) fn listen(name: &str, state: Arc<State>) -> ! {
    let wide: HSTRING = name.into();
    let mut first = true;
    loop {
        match create_instance(&wide, first) {
            Ok(stream) => {
                first = false;
                match wait_client(stream) {
                    Ok(file) => {
                        let state = Arc::clone(&state);
                        std::thread::spawn(move || serve(file, state));
                    }
                    // 客户端在等待中消失;句柄随 File 释放,循环重建实例。
                    Err(_) => {}
                }
            }
            Err(error) if first => {
                // 首实例要求独占创建:失败说明名字已被占用。重复启动该退出,
                // 被抢占更该退出——宁可服务不可用,也不把按键送进可疑对端。
                eprintln!("管道首实例建立失败: {error}。同名管道已被占用(重复启动或被抢占),退出。");
                std::process::exit(1);
            }
            Err(error) => {
                eprintln!("管道实例建立失败: {error},5 秒后重试");
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        }
    }
}

/// 建一个实例。句柄交给 `File` 管:对端关闭时 std 把 `ERROR_BROKEN_PIPE` 当 EOF。
/// `first` 要求独占创建(`FILE_FLAG_FIRST_PIPE_INSTANCE`),首实例抢不到名字即失败。
fn create_instance(name: &HSTRING, first: bool) -> io::Result<File> {
    // 系统应用(UWP/设置/搜索)以低完整性、AppContainer 令牌连管道;默认 DACL
    // 只给当前用户,它们会连不上而变成打不出中文。放行对象所有者(当前用户)、
    // AppContainer 与受限 AppContainer,低完整性标签让低 IL 客户端也能读写;
    // 管道名本身仍带每用户后缀。
    let security = security_attributes();
    // 首实例独占创建:名字被占即失败;后续实例按普通方式补位。
    let open_mode = if first {
        PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
    } else {
        PIPE_ACCESS_DUPLEX
    };
    let handle = unsafe {
        CreateNamedPipeW(
            name,
            open_mode,
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

/// 一次连接识别出来的客户端身份。进程句柄在识别后立即关闭:映像路径
/// 在进程存活期内不变,读完即走,不钉住对方进程对象。
struct ClientIdentity {
    image: String,
    appcontainer: bool,
}

/// 识别管道客户端:映像路径 + 是否 AppContainer。查询失败(客户端刚退出、
/// 权限不足)返回 `None`,调用方对特权请求一律按未授权处理。
fn client_identity(file: &File) -> Option<ClientIdentity> {
    let mut pid = 0u32;
    unsafe { GetNamedPipeClientProcessId(HANDLE(file.as_raw_handle()), &mut pid) }.ok()?;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let imaged = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    let image = if imaged.is_ok() {
        String::from_utf16_lossy(&buf[..len as usize])
    } else {
        String::new()
    };
    let mut token = HANDLE::default();
    let tokened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    let appcontainer = tokened.is_ok() && {
        let mut value = 0u32;
        let mut returned = 0u32;
        unsafe {
            GetTokenInformation(
                token,
                TokenIsAppContainer,
                Some(&mut value as *mut u32 as *mut _),
                size_of::<u32>() as u32,
                &mut returned,
            )
        }
        .is_ok_and(|()| value != 0)
    };
    if tokened.is_ok() {
        unsafe {
            let _ = CloseHandle(token);
        }
    }
    unsafe {
        let _ = CloseHandle(process);
    }
    (imaged.is_ok() && tokened.is_ok()).then_some(ClientIdentity {
        image,
        appcontainer,
    })
}

/// 特权请求的客户端校验;返回拒绝原因。打字流不设限:低完整性的系统
/// 宿主必须能打中文。AppContainer 进程(浏览器渲染器等)再读文件受限,
/// 管道是它们逃逸用户词库的缝,一律挡在 Read 与 Manage 之外;Manage
/// 另要求映像是本产品的工具进程。
fn deny_reason(identity: Option<&ClientIdentity>, request: &Request) -> Option<&'static str> {
    let privilege = request.privilege();
    if privilege == Privilege::Host {
        return None;
    }
    let identity = identity.as_ref()?;
    if identity.appcontainer {
        return Some("AppContainer 客户端");
    }
    if privilege == Privilege::Manage {
        let name = Path::new(&identity.image)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let known = MANAGE_CLIENTS
            .iter()
            .any(|tool| name.eq_ignore_ascii_case(tool));
        if !known {
            return Some("映像不是本产品工具");
        }
    }
    None
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
    // 识别客户端:键盘数据的来源进程,日志排查与特权请求分级用。
    let mut pid = 0u32;
    if unsafe { GetNamedPipeClientProcessId(HANDLE(file.as_raw_handle()), &mut pid) }.is_ok() {
        println!("管道客户端 pid={pid}");
    }
    let identity = client_identity(&file);
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
        let started = std::time::Instant::now();
        let denied = request
            .as_ref()
            .and_then(|req| deny_reason(identity.as_ref(), req));
        let reply = if let Some(reason) = denied {
            let kind = request
                .as_ref()
                .map(|req| format!("{:?}", req.privilege()))
                .unwrap_or_default();
            println!("拒绝 {kind} 请求({reason}):pid={pid}");
            crate::imp::neutral(&state)
        } else {
            request
                .map(|req| {
                    // 单个坏请求不能带崩常驻服务;回一个不改文档、不改模式的中性应答。
                    catch_unwind(AssertUnwindSafe(|| crate::imp::handle(&state, req)))
                        .unwrap_or_else(|payload| {
                            // 服务日志已接到文件,panic 内容要落盘,否则只剩一句「出错了」。
                            let msg = payload
                                .downcast_ref::<&str>()
                                .copied()
                                .or(payload.downcast_ref::<String>().map(String::as_str))
                                .unwrap_or("<非字符串 panic>");
                            eprintln!("请求处理 panic,已回中性应答:{msg}");
                            crate::imp::neutral(&state)
                        })
                })
                .unwrap_or_else(|| crate::imp::neutral(&state))
        };
        let elapsed = started.elapsed().as_millis();
        if elapsed >= SLOW_REQUEST_MS {
            // 行 JSON 的行首就是变体名;只留名字,别把整条按键灌进日志。
            let kind = std::str::from_utf8(&line)
                .map(|text| {
                    text.trim_start_matches('{')
                        .split(':')
                        .next()
                        .unwrap_or("?")
                        .trim_matches('"')
                        .to_string()
                })
                .unwrap_or_else(|_| "?".to_string());
            println!("慢请求 {kind} 用了 {elapsed} ms");
        }
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
