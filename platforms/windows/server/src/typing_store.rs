//! 本地打字记录的落盘:按键路径只把收齐的句子交给通道,写盘在独立线程里做。
//! 记录目录是 `%LOCALAPPDATA%\lueying\typing`,只存本机、从不上传。

use std::path::PathBuf;
use std::sync::mpsc;

use lueying_core::typing_log::{self, Sentence};

/// 记录目录;没有 LOCALAPPDATA 时为 None。只算路径,不建目录。
pub fn dir() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join("lueying")
            .join("typing"),
    )
}

/// 启动落盘线程并返回发送端。通道关闭(进程退出)时线程随之结束。
pub fn spawn() -> mpsc::Sender<Vec<Sentence>> {
    let (tx, rx) = mpsc::channel::<Vec<Sentence>>();
    std::thread::spawn(move || {
        while let Ok(mut batch) = rx.recv() {
            while let Ok(more) = rx.try_recv() {
                batch.extend(more);
            }
            let Some(dir) = dir() else { continue };
            if let Err(error) = typing_log::append(&dir, &batch) {
                println!("打字记录写入失败: {error}");
            }
        }
    });
    tx
}
