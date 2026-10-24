//! 用户词库落盘:学习结果跨重启保留。
//!
//! 写盘在独立线程里按修订号检查,按键路径只碰内存;先写临时文件再改名,
//! 进程中途退出也不会留下半截词库。

use std::path::{Path, PathBuf};
use std::time::Duration;

use pinyin_engine::{SharedUser, UserDict};

const CHECK_EVERY: Duration = Duration::from_secs(1);

/// 用户词库路径(`%LOCALAPPDATA%\pinyin-ime\user.tsv`),目录随之创建。
pub(super) fn path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("pinyin-ime");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("user.tsv"))
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
        .name("pinyin-user-save".into())
        .spawn(move || {
            loop {
                std::thread::sleep(CHECK_EVERY);
                let rev = revision(&user);
                if rev == saved {
                    continue;
                }
                let text = user
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .export();
                match write(&path, &text) {
                    Ok(()) => saved = rev,
                    Err(e) => println!("用户词库 {} 写入失败: {e}", path.display()),
                }
            }
        })
        .expect("启动用户词库保存线程失败");
}

fn revision(user: &SharedUser) -> u64 {
    user.read().unwrap_or_else(|p| p.into_inner()).revision()
}

fn write(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tsv.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}
