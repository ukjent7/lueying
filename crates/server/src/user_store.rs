//! 用户词库落盘:SQLite(WAL)持久化,学习结果跨重启保留。
//!
//! 写盘在独立线程里按修订号检查,按键路径只碰内存;SQLite 事务保证
//! 进程中途退出也不留半截词库。文本导入导出格式仍是词库的交换格式,
//! 整份词库作为单行文档存库,要跨进程读词条时再拆结构化表。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use lueying_engine::{SharedUser, UserDict};
use rusqlite::{Connection, OptionalExtension, params};

const CHECK_EVERY: Duration = Duration::from_secs(1);

/// 保存线程与「清空/退出」的同步落盘不能同时写。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 用户词库路径(`%LOCALAPPDATA%\lueying\user.db`),目录随之创建。
pub(super) fn path() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let dir = base.join("lueying");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("user.db"))
}

/// 读入已有词库;库缺失或损坏都从空开始,旧 TSV 词库一次性接走。
pub(super) fn load(path: &Path) -> UserDict {
    let mut user = UserDict::new();
    // 库还不存在才接旧 TSV:库里已被清空的词库不能被旧文件复活。
    let legacy = (!path.exists()).then(legacy_text).flatten();
    match open(path) {
        Ok(conn) => match doc(&conn) {
            Ok(Some(text)) => {
                user.import(&text);
                println!("用户词库 ← {}", path.display());
            }
            Ok(None) => {
                if let Some(text) = legacy {
                    user.import(&text);
                    if let Err(e) = write(&conn, &user.export()) {
                        println!("用户词库 {} 迁移写入失败: {e}", path.display());
                    } else {
                        println!("用户词库 ← 旧版 TSV,已迁入 {}", path.display());
                    }
                }
            }
            Err(e) => println!("用户词库 {} 读取失败: {e}", path.display()),
        },
        Err(e) => println!("用户词库 {} 打开失败: {e}", path.display()),
    }
    user
}

/// 旧版 TSV 词库文本:先 `lueying\user.tsv`,再改名前的 `pinyin-ime\user.tsv`。
fn legacy_text() -> Option<String> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let tsv = base.join("lueying").join("user.tsv");
    let text = std::fs::read_to_string(&tsv).ok().or_else(|| {
        // 更早版本的词库在另一个目录,复制过来统一从那里迁。
        let old = base.join("pinyin-ime").join("user.tsv");
        std::fs::copy(&old, &tsv).ok()?;
        std::fs::read_to_string(&tsv).ok()
    })?;
    (!text.is_empty()).then_some(text)
}

/// 每秒检查一次修订号,有变化就整份写出。
pub(super) fn spawn(user: SharedUser, path: PathBuf) {
    // 在起线程前取基准,避免线程启动晚于第一次学习而漏存。
    let mut saved = revision(&user);
    std::thread::Builder::new()
        .name("lueying-user-save".into())
        .spawn(move || loop {
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
    if let Err(e) = open(path).and_then(|conn| write(&conn, &text)) {
        println!("用户词库 {} 写入失败: {e}", path.display());
    }
}

/// 修订号仍然是 `rev` 才写,避免保存线程用旧快照覆盖刚清空的词库。
fn write_if_current(user: &SharedUser, path: &Path, rev: u64) -> rusqlite::Result<bool> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    if revision(user) != rev {
        return Ok(false);
    }
    let text = user.read().unwrap_or_else(|p| p.into_inner()).export();
    let conn = open(path)?;
    write(&conn, &text)?;
    Ok(true)
}

/// 打开连接并建库:WAL 模式,单行文档表。
fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS doc (
            id   INTEGER PRIMARY KEY CHECK (id = 1),
            text TEXT NOT NULL
        )",
    )?;
    Ok(conn)
}

/// 读出整份词库文本;空库返回 `None`。
fn doc(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT text FROM doc WHERE id = 1", [], |row| row.get(0))
        .optional()
}

/// 整份写入;upsert 单行文档。
fn write(conn: &Connection, text: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO doc (id, text) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET text = excluded.text",
        params![text],
    )?;
    Ok(())
}
