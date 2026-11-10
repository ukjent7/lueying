//! 与服务端的 stdio 通道。出站由所有线程共享,一次写一整行;入站由读线程逐行解码。

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use lueying_ext::proto::link::{ToHost, ToServer, decode, encode};

/// 出站写端。服务端不读时写会阻塞,阻塞的只是调用它的线程,不影响服务端。
#[derive(Clone)]
pub struct Outbound(Arc<Mutex<std::io::Stdout>>);

impl Outbound {
    /// 接管标准输出。
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(std::io::stdout())))
    }

    /// 发一条消息;编码或写失败(服务端已关)时静默丢弃。
    pub fn send(&self, message: &ToServer) {
        let Ok(line) = encode(message) else {
            return;
        };
        let mut out = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// 逐行读标准输入直到 EOF(服务端关闭即宿主退出)。坏行记到标准错误后跳过。
pub fn read_lines(mut each: impl FnMut(ToHost)) {
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            return;
        };
        match decode::<ToHost>(&line) {
            Ok(message) => each(message),
            Err(error) => eprintln!("扩展宿主收到坏行: {error}"),
        }
    }
}
