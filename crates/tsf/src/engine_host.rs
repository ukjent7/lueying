//! 引擎宿主:词典加载与 panic 兜底。

use std::panic::{AssertUnwindSafe, catch_unwind};

use pinyin_dict::Dictionary;
use pinyin_engine::{Config, Engine, Event, Key};

use crate::log;

pub(crate) fn load_engine() -> Engine {
    let dict = dict_path()
        .and_then(|p| Dictionary::from_path(&p).ok())
        .map(|(d, bad)| {
            log(&format!("词典加载 {} 条(跳过坏行 {bad})", d.len()));
            d
        })
        .unwrap_or_else(|| {
            log("主词典缺失,退回内置迷你词库");
            Dictionary::builtin()
        });
    Engine::new(dict, Config::default())
}

/// DLL 同目录的 main.tsv。
fn dict_path() -> Option<std::path::PathBuf> {
    let mut path = crate::module_path_file()?;
    path.set_file_name("main.tsv");
    Some(path)
}

/// 引擎按键的 panic 兜底:引擎任何异常都不能崩宿主。
pub(crate) fn safe_process(engine: &mut Engine, key: Key) -> Vec<Event> {
    match catch_unwind(AssertUnwindSafe(|| engine.process(key))) {
        Ok(events) => events,
        Err(_) => {
            log("引擎处理按键 panic,已兜住");
            Vec::new()
        }
    }
}
