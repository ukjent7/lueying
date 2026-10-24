//! 命名管道监听:多实例阻塞模式,每连接一个线程,行 JSON 请求-应答。

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
use windows::Win32::System::Pipes::{ConnectNamedPipeW, CreateNamedPipeW};

use crate::imp::State;
use pinyin_protocol::Request;

/// 阻塞主线程:循环建管道实例并等待连接。永不返回。
pub(super) fn listen(name: &str, state: Arc<State>) -> ! {
    loop {
        let handle = create_instance(name);
        if handle == INVALID_HANDLE_VALUE {
            eprintln!("CreateNamedPipeW 失败,5 秒后重试");
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        }
        let connected = unsafe { ConnectNamedPipeW(handle, None) }.as_bool();
        if !connected && unsafe { windows::Win32::Foundation::GetLastError() } != ERROR_PIPE_CONNECTED {
            unsafe { let _ = CloseHandle(handle) };
            continue;
        }
        let file = unsafe { File::from_raw_handle(handle.0 as _) };
        let state = Arc::clone(&state);
        std::thread::spawn(move || serve(file, state));
    }
}

fn create_instance(name: &str) -> windows::Win32::Foundation::HANDLE {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            windows::Win32::System::Pipes::PIPE_ACCESS_DUPLEX,
            windows::Win32::System::Pipes::PIPE_READMODE_BYTE
                | windows::Win32::System::Pipes::PIPE_WAIT,
            windows::Win32::System::Pipes::PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    }
}

fn serve(file: File, state: Arc<State>) {
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
        let reply = serde_json_parse(&line)
            .map(|req| crate::imp::handle(&state, req))
            .unwrap_or_default();
        let mut out = serde_json::to_string(&reply).unwrap_or_default();
        out.push('\n');
        if writer.write_all(out.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
    }
}

/// 解析一行请求。
fn serde_json_parse(line: &str) -> Option<Request> {
    serde_json::from_str(line.trim()).ok()
}
