//! 命名管道监听:多实例阻塞模式,每连接一个线程,行 JSON 请求-应答。
//! 建法与青简 server 相同:CreateNamedPipeW + ConnectNamedPipe,句柄交给 `File` 管。

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::Arc;

use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE, HRESULT};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::core::HSTRING;

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
    let handle = unsafe {
        CreateNamedPipeW(
            name,
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_SIZE,
            BUFFER_SIZE,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle.0) })
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
            .map(|req| crate::imp::handle(&state, req))
            .unwrap_or_default();
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
