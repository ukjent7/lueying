//! 单实例守卫与 exe 同目录数据装载:重排模型、主词典与主 gram。
//!
//! 纯装载逻辑,不碰共享状态;进程启动与配置切换都走这一组。

use super::*;

/// 当前会话的单实例锁;返回的句柄要活到进程结束。
pub(super) fn single_instance() -> Option<HANDLE> {
    let pipe = pipe_name();
    let short = pipe.trim_start_matches(r"\\.\pipe\");
    let name = HSTRING::from(format!("Local\\{short}"));
    let handle = unsafe { CreateMutexW(None, true, &name) }.ok()?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return None;
    }
    Some(handle)
}

/// exe 同目录的数据文件(main.pyd / main.tsv / main.gram / rerank-*.*)。
pub(super) fn dict_path(name: &str) -> Option<std::path::PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.set_file_name(name);
    path.exists().then_some(path)
}

pub(super) fn load_model(stem: &str) -> Option<Model> {
    let mut dirs = Vec::new();
    if let Some(dir) = config::models_dir() {
        dirs.push(dir);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_path_buf());
    }
    for dir in dirs {
        if !dir.join(format!("{stem}.ort")).is_file() {
            continue;
        }
        match Model::load_keystroke_dir(&dir, stem) {
            Ok(model) => {
                println!("重排模型 {stem} ← {}", dir.display());
                return Some(model);
            }
            Err(e) => println!("重排模型 {stem} 不可用: {e}"),
        }
    }
    None
}

pub(super) fn load_dict(path: std::path::PathBuf) -> Option<Dictionary> {
    match Dictionary::from_path(&path) {
        Ok((d, 0)) if !d.is_empty() => {
            println!("词典 {} 条 ← {}", d.len(), path.display());
            Some(d)
        }
        Ok((_, bad)) => {
            println!("{} 有 {bad} 处问题,跳过", path.display());
            None
        }
        Err(e) => {
            println!("{} 加载失败: {e}", path.display());
            None
        }
    }
}
