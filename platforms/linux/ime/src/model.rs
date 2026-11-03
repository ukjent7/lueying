//! 共享数据:词典、语法模型与神经重排模型。
//!
//! 进程内只加载一次;查找顺序是显式数据目录、安装目录旁的 `share/lueying`
//! 与 `data`。都没有时退回内置词典,输入法仍可用。

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use lueying_dict::Dictionary;
use lueying_dict::gram::Gram;
use lueying_rerank::Model;

/// 数据目录候选,按优先级排列。
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("LUEYING_DATA_DIR") {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.join("../share/lueying"));
        dirs.push(parent.join("data"));
    }
    dirs.push(PathBuf::from("/usr/local/share/lueying"));
    dirs.push(PathBuf::from("/usr/share/lueying"));
    dirs.push(PathBuf::from("data"));
    dirs
}

pub fn shared_dictionary() -> Arc<Dictionary> {
    static DICTIONARY: OnceLock<Arc<Dictionary>> = OnceLock::new();
    Arc::clone(DICTIONARY.get_or_init(|| Arc::new(load_dictionary())))
}

pub fn shared_gram() -> Option<Arc<Gram>> {
    static GRAM: OnceLock<Option<Arc<Gram>>> = OnceLock::new();
    GRAM.get_or_init(|| {
        data_dirs()
            .into_iter()
            .find_map(|dir| Gram::open(&dir.join("main.gram")).ok())
            .map(Arc::new)
    })
    .clone()
}

/// 载入一档重排模型;文件不存在就返回 `None`,会话自动只用引擎排序。
pub fn rerank(name: &str) -> Option<Model> {
    if let Some(dir) = crate::config::models_dir() {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            return Model::load(&bytes).ok();
        }
    }
    let bytes = data_dirs()
        .into_iter()
        .find_map(|dir| std::fs::read(dir.join(name)).ok())?;
    Model::load(&bytes).ok()
}

fn load_dictionary() -> Dictionary {
    for dir in data_dirs() {
        let pyd = dir.join("main.pyd");
        if let Ok(bytes) = std::fs::read(&pyd)
            && let Some(dictionary) = Dictionary::from_binary(&bytes)
        {
            return dictionary;
        }
        let tsv = dir.join("main.tsv");
        if let Ok((dictionary, _)) = Dictionary::from_path(&tsv) {
            return dictionary;
        }
    }
    Dictionary::builtin()
}
