//! Server 管道客户端:懒连接;断线时上层直接放行字母(宁可英文直通也不死键盘)。

use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lueying_protocol::{Reply, Request};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};
use windows::Win32::System::Pipes::{GetNamedPipeServerProcessId, PeekNamedPipe};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

pub(crate) struct Client {
    reader: File,
    /// 已读到但还没凑成完整行的字节(管道是字节流)。
    pending: Vec<u8>,
    writer: File,
}

/// 单次请求-应答的上限。Server 卡住时不能让宿主 UI 线程无限等;
/// 超时按断线处理,下一次按键重新连接。
const ROUND_TRIP_TIMEOUT: Duration = Duration::from_millis(1500);
/// 一行协议的上限,防止坏对端把内存撑爆。
const MAX_LINE: usize = 1024 * 1024;

/// 校验管道对端是 lueying-server:抢占了管道名的进程连按键都拿不到。
/// 查不到服务端 PID 或映像名不符一律视为假服务。
fn verify_server(file: &File) -> bool {
    let mut pid = 0u32;
    let Ok(()) = (unsafe { GetNamedPipeServerProcessId(HANDLE(file.as_raw_handle()), &mut pid) })
    else {
        return false;
    };
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return false;
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    }
    .is_ok();
    unsafe { let _ = CloseHandle(process); }
    ok
        && Path::new(String::from_utf16_lossy(&buf[..len as usize]).as_str())
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("lueying-server.exe"))
}

impl Client {
    pub(crate) fn connect() -> Option<Self> {
        if let Some(client) = Self::open() {
            WARMING.store(false, Ordering::Relaxed);
            return Some(client);
        }
        // Server 没在跑:拉起安装目录里的兄弟进程,等它建好管道。
        // 冷启动可能超过一轮等待,3 秒内继续等;超过后回到每 10 秒一次的
        // 拉起节流,避免每个按键都陪等服务启动。
        let warming = WARMING.load(Ordering::Relaxed);
        let started = if warming { false } else { spawn_server_once() };
        let recent = now_ms().saturating_sub(LAST_SPAWN.load(Ordering::Relaxed)) < STARTUP_GRACE_MS;
        if !warming && !started && !recent {
            return None;
        }
        // 刚拉起(含激活时预热)说明服务正在加载词典/模型。首键直接放行会
        // 少一个字母,后续键还会把拼音接错;等不到就清标记。
        WARMING.store(false, Ordering::Relaxed);
        for _ in 0..16 {
            std::thread::sleep(Duration::from_millis(15));
            if let Some(client) = Self::open() {
                return Some(client);
            }
        }
        None
    }

    fn open() -> Option<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            // SQOS=IDENTIFICATION:客户端禁止管道服务端模拟自己——键盘流是敏感
            // 数据,mozc/WeType 同款。custom_flags 正好并入 dwFlagsAndAttributes。
            .custom_flags(SECURITY_SQOS_PRESENT.0 | SECURITY_IDENTIFICATION.0)
            .open(lueying_protocol::pipe_name())
            .ok()?;
        // 对端身份校验:管道名可被抢先创建,连上后确认服务端真是
        // lueying-server;不是就当服务不在(按键按断线放行),绝不外发。
        if !verify_server(&file) {
            return None;
        }
        let writer = file.try_clone().ok()?;
        Some(Self {
            reader: file,
            pending: Vec::new(),
            writer,
        })
    }

    /// 一次请求-应答;任何一步失败或超时都返回 None(上层视作断线)。
    pub(crate) fn round_trip(&mut self, req: &Request) -> Option<Reply> {
        let mut line = serde_json::to_string(req).ok()?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).ok()?;
        self.writer.flush().ok()?;
        let deadline = Instant::now() + ROUND_TRIP_TIMEOUT;
        loop {
            if let Some(line) = take_line(&mut self.pending) {
                if line.is_empty() {
                    continue;
                }
                return serde_json::from_str(&line).ok();
            }
            if self.pending.len() > MAX_LINE {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let mut available = 0u32;
            let peek = unsafe {
                PeekNamedPipe(
                    HANDLE(self.reader.as_raw_handle()),
                    None,
                    0,
                    None,
                    Some(&mut available),
                    None,
                )
            };
            if peek.is_err() {
                return None;
            }
            if available == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            let mut buf = [0u8; 4096];
            let read = self.reader.read(&mut buf).ok()?;
            if read == 0 {
                return None;
            }
            self.pending.extend_from_slice(&buf[..read]);
        }
    }
}

/// 从缓冲区取出一行(不含换行符);没有完整行时返回 None。
fn take_line(pending: &mut Vec<u8>) -> Option<String> {
    let end = pending.iter().position(|&byte| byte == b'\n')?;
    let line: Vec<u8> = pending.drain(..=end).collect();
    String::from_utf8(line).ok().map(|line| {
        line.trim_end_matches(|c| c == '\r' || c == '\n')
            .to_string()
    })
}

/// 激活输入法时先拉起 Server,让词典与重排模型在用户敲第一键前开始加载。
/// 只起进程不建连接:真正的 Hello 仍由首键按需完成。
pub(crate) fn warm_server() {
    let _ = spawn_server_once();
}

/// 已经连上的 Server 掉线:清掉拉起节流,下一次连接立即尝试重启。
pub(crate) fn note_disconnect() {
    LAST_SPAWN.store(0, Ordering::Relaxed);
}

/// 每个宿主进程每 10 秒最多拉起一次 Server;正常注入的 DLL 与 Server 同目录。
/// 只有本次真的发起了进程才返回 true,调用方据此决定要不要等管道;
/// 刚起过或同目录没有 Server 都立即返回,键盘不用等。
fn spawn_server_once() -> bool {
    const RETRY_MS: u64 = 10_000;
    let Some(dir) =
        crate::module_path_file().and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    else {
        return false;
    };
    let exe = dir.join("lueying-server.exe");
    if !exe.exists() {
        return false;
    }
    let now = now_ms();
    let last = LAST_SPAWN.load(Ordering::Relaxed);
    if now.saturating_sub(last) < RETRY_MS
        || LAST_SPAWN
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return false;
    }
    // 不继承宿主句柄:宿主若用管道读输出,常驻服务会把管道写端一直占住。
    let started = lueying_protocol::spawn_detached(&exe, None).is_ok();
    if started {
        WARMING.store(true, Ordering::Relaxed);
    }
    started
}

/// 发起进程后继续等管道的窗口。冷启动加载词典/模型可能超过单轮 240ms,
/// 这段时间内的按键都要继续尝试连接,不能立刻放行成英文。
const STARTUP_GRACE_MS: u64 = 3_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

/// 上次尝试拉起 Server 的毫秒时间戳。
static LAST_SPAWN: AtomicU64 = AtomicU64::new(0);
/// 刚发起过 Server 进程、还没确认管道就绪;下一次连接最多等一轮。
static WARMING: AtomicBool = AtomicBool::new(false);
