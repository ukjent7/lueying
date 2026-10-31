//! 用户词库落盘:学习结果跨重启保留。
//!
//! 写盘在独立线程里按修订号检查,按键路径只碰内存;先写临时文件再改名,
//! 进程中途退出也不会留下半截词库。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use lueying_engine::{SharedUser, UserDict};

const CHECK_EVERY: Duration = Duration::from_secs(1);

/// 保存线程与「清空/退出」的同步落盘不能同时写同一个临时文件。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 用户词库路径(`%LOCALAPPDATA%\lueying\user.tsv`),目录随之创建。
pub(super) fn path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let dir = base.join("lueying");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("user.tsv");
    // 一次性接走改名前的词库,升级后学习结果不丢。
    if !path.exists() {
        let old = base.join("pinyin-ime").join("user.tsv");
        if old.is_file() {
            let _ = std::fs::copy(&old, &path);
        }
    }
    Some(path)
}

/// 读入已有词库;文件缺失或个别坏行都不影响启动。
pub(super) fn load(path: &Path) -> UserDict {
    let mut user = UserDict::new();
    match std::fs::read_to_string(path) {
        Ok(text) => {
            user.import(&text);
            println!("用户词库 ← {}", path.display());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => println!("用户词库 {} 读取失败: {e}", path.display()),
    }
    user
}

/// 每秒检查一次修订号,有变化就整份写出。
pub(super) fn spawn(user: SharedUser, path: PathBuf) {
    // 在起线程前取基准,避免线程启动晚于第一次学习而漏存。
    let mut saved = revision(&user);
    std::thread::Builder::new()
        .name("lueying-user-save".into())
        .spawn(move || {
            loop {
                std::thread::sleep(CHECK_EVERY);
                let rev = revision(&user);
                if rev == saved {
                    continue;
                }
                // 拿到写锁后再确认修订号:清空/退出刚写过时不能把旧内容盖回去。
                match write_if_current(&user, &path, rev) {
                    Ok(true) => saved = rev,
                    Ok(false) => {}
                    Err(e) => println!("用户词库 {} 写入失败: {e}", path.display()),
                }
            }
        })
        .expect("启动用户词库保存线程失败");
}

fn revision(user: &SharedUser) -> u64 {
    user.read().unwrap_or_else(|p| p.into_inner()).revision()
}

/// 立即整份写出(清空词库、退出前),不等待每秒检查。
pub(super) fn flush(user: &SharedUser, path: &Path) {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let text = user.read().unwrap_or_else(|p| p.into_inner()).export();
    if let Err(e) = write_unlocked(path, &text) {
        println!("用户词库 {} 写入失败: {e}", path.display());
    }
}

/// 修订号仍然是 `rev` 才写,避免保存线程用旧快照覆盖刚清空的词库。
fn write_if_current(user: &SharedUser, path: &Path, rev: u64) -> std::io::Result<bool> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if revision(user) != rev {
        return Ok(false);
    }
    let text = user.read().unwrap_or_else(|p| p.into_inner()).export();
    write_unlocked(path, &text)?;
    Ok(true)
}

fn write_unlocked(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tsv.tmp");
    // 先 fsync 再改名:断电时最多留下旧词库,不会把空文件或半截文本当词库。
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}
