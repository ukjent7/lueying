//! Server 管道客户端:懒连接 + 失败退避;断线时上层直接放行字母(宁可英文直通也不死键盘)。

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pinyin_protocol::{Reply, Request};
use windows::Win32::Storage::FileSystem::{SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT};

pub(crate) struct Client {
    reader: BufReader<File>,
    writer: File,
}

/// 连不上时两次尝试之间的退避(每次尝试都在应用 UI 线程上)。
pub(crate) const RETRY_INTERVAL: Duration = Duration::from_secs(2);

impl Client {
    pub(crate) fn connect() -> Option<Self> {
        if let Some(client) = Self::open() {
            return Some(client);
        }
        // Server 没在跑:拉起安装目录里的兄弟进程,等它建好管道。
        maybe_spawn_server();
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(30));
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
            .open(pinyin_protocol::pipe_name())
            .ok()?;
        let writer = file.try_clone().ok()?;
        Some(Self {
            reader: BufReader::new(file),
            writer,
        })
    }

    /// 一次请求-应答;任何一步失败都返回 None(上层视作断线)。
    pub(crate) fn round_trip(&mut self, req: &Request) -> Option<Reply> {
        let mut line = serde_json::to_string(req).ok()?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).ok()?;
        self.writer.flush().ok()?;
        let mut buf = String::new();
        self.reader.read_line(&mut buf).ok()?;
        serde_json::from_str(buf.trim()).ok()
    }
}

/// 每个宿主进程每 10 秒最多拉起一次 Server;正常注入的 DLL 与 Server 同目录。
fn maybe_spawn_server() {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const RETRY_SECS: u64 = 10;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let last = LAST_SPAWN.load(Ordering::Relaxed);
    if now.saturating_sub(last) < RETRY_SECS
        || LAST_SPAWN
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    let Some(dir) =
        crate::module_path_file().and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    else {
        return;
    };
    let exe = dir.join("pinyin-server.exe");
    if !exe.exists() {
        return;
    }
    let _ = std::process::Command::new(exe)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// 上次尝试拉起 Server 的秒级时间戳。
static LAST_SPAWN: AtomicU64 = AtomicU64::new(0);
