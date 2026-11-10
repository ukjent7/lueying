//! 扩展私有存储:每个扩展一个 JSON 文件,配额受限。

use std::path::PathBuf;

use lueying_ext::host::{Done, Provider};

use serde_json::json;

/// 扩展私有存储:每个扩展一个 JSON 文件,调用方由 `Provider::call` 的
/// `caller` 决定,不能读写其它扩展的数据。键与扩展 id 同一套字符规则,
/// 单值上限 64 KiB,整份文件上限 256 KiB。
pub struct StoreProvider;

/// 单个值的上限;扩展存的是设置与缓存,不该把用户磁盘当数据库。
const STORE_VALUE_LIMIT: usize = 64 * 1024;
/// 每个扩展整份存储的上限。
const STORE_FILE_LIMIT: usize = 256 * 1024;

impl Provider for StoreProvider {
    fn call(&self, caller: &str, method: &str, args: &str, done: Done) -> Result<(), String> {
        if !lueying_ext::manifest::valid_id(caller) {
            done.complete(Err("扩展 id 不合法".to_string()));
            return Ok(());
        }
        let result = match method {
            "get" => store_get(caller, args),
            "set" => store_set(caller, args),
            "remove" => store_remove(caller, args),
            "list" => store_list(caller),
            _ => Err(format!("store 没有方法 {method:?}")),
        };
        done.complete(result);
        Ok(())
    }
}

fn store_get(ext: &str, args: &str) -> Result<String, String> {
    let key = store_key(args)?;
    let values = store_load(ext);
    let value = values.get(&key).and_then(serde_json::Value::as_str);
    Ok(json!({ "value": value }).to_string())
}

fn store_set(ext: &str, args: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
    let key = parsed["key"].as_str().ok_or_else(|| "缺 key".to_string())?;
    if !lueying_ext::manifest::valid_id(key) {
        return Err("存储键不合法".to_string());
    }
    let value = parsed["value"]
        .as_str()
        .ok_or_else(|| "缺 value".to_string())?;
    if value.len() > STORE_VALUE_LIMIT {
        return Err(format!("存储值超过 {STORE_VALUE_LIMIT} 字节"));
    }
    let mut values = store_load(ext);
    values.insert(
        key.to_string(),
        serde_json::Value::String(value.to_string()),
    );
    store_save(ext, &values)?;
    Ok("{}".to_string())
}

fn store_remove(ext: &str, args: &str) -> Result<String, String> {
    let key = store_key(args)?;
    let mut values = store_load(ext);
    values.remove(&key);
    store_save(ext, &values)?;
    Ok("{}".to_string())
}

fn store_list(ext: &str) -> Result<String, String> {
    let keys: Vec<String> = store_load(ext).into_iter().map(|(key, _)| key).collect();
    Ok(json!({ "keys": keys }).to_string())
}

fn store_key(args: &str) -> Result<String, String> {
    let parsed: serde_json::Value = serde_json::from_str(args).map_err(|e| e.to_string())?;
    let key = parsed["key"].as_str().ok_or_else(|| "缺 key".to_string())?;
    if !lueying_ext::manifest::valid_id(key) {
        return Err("存储键不合法".to_string());
    }
    Ok(key.to_string())
}

fn store_load(ext: &str) -> serde_json::Map<String, serde_json::Value> {
    let Some(path) = store_path(ext) else {
        return serde_json::Map::new();
    };
    std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn store_save(
    ext: &str,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let path = store_path(ext).ok_or("没有可写的扩展存储目录")?;
    let Some(dir) = path.parent() else {
        return Err("扩展存储路径没有父目录".to_string());
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("创建扩展存储目录: {e}"))?;
    let text = serde_json::to_vec(values).map_err(|e| format!("序列化扩展存储: {e}"))?;
    if text.len() > STORE_FILE_LIMIT {
        return Err(format!("扩展存储超过 {STORE_FILE_LIMIT} 字节"));
    }
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("创建扩展存储: {e}"))?;
    std::io::Write::write_all(&mut file, &text).map_err(|e| format!("写扩展存储: {e}"))?;
    file.sync_all().map_err(|e| format!("同步扩展存储: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("替换扩展存储: {e}"))
}

fn store_path(ext: &str) -> Option<PathBuf> {
    Some(crate::state::paths().ext_data().join(format!("{ext}.json")))
}
