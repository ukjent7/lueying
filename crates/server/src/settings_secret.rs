//! 扩展密钥存储:清单里声明为 `secureText` 的值不进 `config.toml`。
//!
//! 每个扩展一个 JSON 文件,放在 `%APPDATA%\lueying\secrets`;目录与文件
//! 在 Unix 上分别设 0700/0600,在 Windows 上依赖 `%APPDATA%` 的当前用户
//! ACL。写入先落 `.tmp`、`sync_all` 后再改名,断电不会留下半截密钥文件。
//! 这里只按需读写,不在日志与错误信息里回显值。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::config;

/// 读取一个密钥;没有或文件损坏时返回 None。
pub fn get(ext: &str, key: &str) -> Option<String> {
    table(ext)
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// 是否已经设置过某个密钥。
pub fn has(ext: &str, key: &str) -> bool {
    get(ext, key).is_some_and(|value| !value.is_empty())
}

/// 设置一个密钥;空串等价于删除。
pub fn set(ext: &str, key: &str, value: &str) -> Result<(), String> {
    if !lueying_extmeta::valid_id(ext) || !lueying_extmeta::valid_id(key) {
        return Err("扩展或设置键不合法".to_string());
    }
    let mut values = table(ext);
    if value.is_empty() {
        values.remove(key);
    } else {
        if value.chars().count() > lueying_extmeta::MAX_TEXT_CHARS {
            return Err("密钥过长".to_string());
        }
        values.insert(key.to_string(), Value::String(value.to_string()));
    }
    save(ext, &values)
}

/// 删除一个密钥;文件里没有其它值时连文件一起删掉。
pub fn remove(ext: &str, key: &str) -> Result<(), String> {
    let mut values = table(ext);
    values.remove(key);
    save(ext, &values)
}

/// 把某扩展的密钥并进设置值的对象;只写声明过 `secureText` 的键。
pub fn merge(ext: &str, settings: &[lueying_extmeta::Setting], object: &mut Map<String, Value>) {
    let values = table(ext);
    for setting in settings.iter().filter(|setting| setting.is_secret()) {
        if let Some(value) = values.get(setting.key()).and_then(Value::as_str) {
            object.insert(
                setting.key().to_string(),
                Value::String(value.to_string()),
            );
        }
    }
}

/// 某扩展当前已设置的密钥键;供设置页显示「已设置」状态。
pub fn keys(ext: &str) -> BTreeMap<String, bool> {
    table(ext)
        .into_iter()
        .filter_map(|(key, value)| value.as_str().map(|text| (key, !text.is_empty())))
        .collect()
}

fn table(ext: &str) -> Map<String, Value> {
    let Some(path) = path(ext) else {
        return Map::new();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Map::new();
    };
    serde_json::from_slice::<Map<String, Value>>(&bytes).unwrap_or_default()
}

fn save(ext: &str, values: &Map<String, Value>) -> Result<(), String> {
    let path = path(ext).ok_or("没有可写的密钥目录")?;
    let Some(dir) = path.parent() else {
        return Err("密钥路径没有父目录".to_string());
    };
    ensure_dir(dir)?;
    if values.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let text = serde_json::to_vec_pretty(values).map_err(|error| format!("序列化密钥: {error}"))?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp).map_err(|error| format!("创建密钥文件: {error}"))?;
        std::io::Write::write_all(&mut file, &text)
            .map_err(|error| format!("写密钥文件: {error}"))?;
        file.sync_all().map_err(|error| format!("同步密钥文件: {error}"))?;
    }
    secure_file(&tmp)?;
    std::fs::rename(&tmp, &path).map_err(|error| format!("替换密钥文件: {error}"))?;
    secure_file(&path)
}

fn path(ext: &str) -> Option<PathBuf> {
    if !lueying_extmeta::valid_id(ext) {
        return None;
    }
    Some(config::secrets_dir()?.join(format!("{ext}.json")))
}

fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("创建密钥目录: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn secure_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("设置密钥文件权限: {error}"))?;
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
#[path = "settings_secret_tests.rs"]
mod tests;
