//! Server 管道客户端:懒连接 + 失败退避;断线时上层直接放行字母(宁可英文直通也不死键盘)。

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use pinyin_protocol::{Reply, Request};

pub(crate) struct Client {
    reader: BufReader<File>,
    writer: File,
}

/// 连不上时两次尝试之间的退避(每次尝试都在应用 UI 线程上)。
pub(crate) const RETRY_INTERVAL: Duration = Duration::from_secs(2);

impl Client {
    pub(crate) fn connect() -> Option<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(pinyin_protocol::PIPE_NAME)
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
