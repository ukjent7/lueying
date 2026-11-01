//! `.luext` 扩展包容器:一个文件装下清单、Wasm 模块或可执行文件与数据。
//!
//! 格式(`LUEXT` v1,小端):5 字节魔数 + 1 字节版本 + u32 文件数,之后
//! 每个文件是「u16 名字长 + UTF-8 名字 + u64 内容长 + 内容」。没有压缩
//! 与元数据——扩展包本来就小,大文件(模型)走目录安装,不走容器。
//!
//! 名字必须是裸文件名且不得是 `consent.json`:容器落盘到扩展目录,
//! 同意记录只由宿主写,包里出现同名文件一律拒绝。

const MAGIC: &[u8; 5] = b"LUEXT";
const VERSION: u8 = 1;

/// 包内最多多少个文件。
pub const MAX_FILES: usize = 64;
/// 单个文件名的字节上限。
pub const MAX_NAME: usize = 96;
/// 单个文件内容的字节上限;与同意指纹的读入上限一致。
pub const MAX_FILE_BYTES: usize = 128 * 1024 * 1024;
/// 全包内容合计的字节上限。
pub const MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

/// 容器里的一个文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// 裸文件名校验;`consent.json` 是宿主专属,包里出现即拒。
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(format!("文件名长度不对: {name:?}"));
    }
    if name == "consent.json"
        || name.contains(['/', '\\', '\0'])
        || name.starts_with('.')
    {
        return Err(format!("文件名必须是裸文件名: {name:?}"));
    }
    Ok(())
}

/// 打包;名字与大小在写入前校验,坏条目整包拒绝。
pub fn pack(entries: &[Entry]) -> Result<Vec<u8>, String> {
    if entries.is_empty() || entries.len() > MAX_FILES {
        return Err(format!("文件数要在 1..={MAX_FILES} 之间"));
    }
    let mut total = 0usize;
    for (index, entry) in entries.iter().enumerate() {
        check_name(&entry.name)?;
        if entries[..index].iter().any(|other| other.name == entry.name) {
            return Err(format!("包内文件重名: {}", entry.name));
        }
        total += entry.bytes.len();
        if entry.bytes.len() > MAX_FILE_BYTES {
            return Err(format!("{} 超过 {MAX_FILE_BYTES} 字节", entry.name));
        }
    }
    if total > MAX_TOTAL_BYTES {
        return Err(format!("包内容合计超过 {MAX_TOTAL_BYTES} 字节"));
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        let name = entry.name.as_bytes();
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&(entry.bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&entry.bytes);
    }
    Ok(out)
}

/// 解包;魔数、版本、名字与大小全部复核,坏包整体拒绝。
pub fn unpack(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let bad = |why: &str| format!("不是有效的 .luext 包: {why}");
    if bytes.len() < 10 || &bytes[..5] != MAGIC {
        return Err(bad("魔数不对"));
    }
    if bytes[5] != VERSION {
        return Err(bad("格式版本不支持"));
    }
    let mut cursor = 6usize;
    let mut take = |need: usize| -> Result<&[u8], String> {
        let rest = bytes.get(cursor..).ok_or_else(|| bad("内容被截断"))?;
        if rest.len() < need {
            return Err(bad("内容被截断"));
        }
        cursor += need;
        Ok(&rest[..need])
    };
    let count = {
        let raw = take(4)?;
        let count = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
        if count == 0 || count > MAX_FILES {
            return Err(bad("文件数超限"));
        }
        count
    };
    let mut entries = Vec::with_capacity(count);
    let mut total = 0usize;
    for _ in 0..count {
        let name_len = u16::from_le_bytes(take(2)?.try_into().unwrap()) as usize;
        let name = std::str::from_utf8(take(name_len)?).map_err(|_| bad("文件名不是 UTF-8"))?;
        check_name(name).map_err(|error| bad(&error))?;
        let raw = take(8)?;
        let len = u64::from_le_bytes(raw.try_into().unwrap()) as usize;
        if len > MAX_FILE_BYTES {
            return Err(bad("单个文件超过大小上限"));
        }
        total += len;
        if total > MAX_TOTAL_BYTES {
            return Err(bad("包内容合计超过大小上限"));
        }
        entries.push(Entry {
            name: name.to_string(),
            bytes: take(len)?.to_vec(),
        });
        if entries[..entries.len() - 1]
            .iter()
            .any(|other| other.name == name)
        {
            return Err(bad("包内文件重名"));
        }
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "container_tests.rs"]
mod tests;
