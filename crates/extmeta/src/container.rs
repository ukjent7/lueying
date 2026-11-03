//! `### lueying-pack v2` 扩展包容器:文本块按行数声明长度,Wasm 按
//! 字节数原样内嵌,整个包是可 diff 的 UTF-8 文本。
//!
//! 容器只管「文件集 ⇄ 字节流」;目录收集与清单校验在调用方(cli 的
//! `ext pack`、设置页的「导入」)。授权面约束:包内路径必须是相对
//! 路径,`consent.json` 只由宿主写,包里出现即拒。

use std::path::Path;

pub const PACK_HEADER: &str = "### lueying-pack v2";

/// 二进制文件后缀;Wasm 与原生扩展产物按字节块打包。
const BINARY_EXTS: &[&str] = &[
    "wasm", "exe", "dll", "onnx", "png", "jpg", "jpeg", "webp", "gif", "wav", "mp3", "ogg", "ttf",
    "otf",
];

/// 容器里的一个文件:`(包内相对路径, 内容)`。
pub type Files = Vec<(String, Vec<u8>)>;

/// 包内路径白名单:相对路径、不越界、不写隐藏文件,`consent.json` 拒收。
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "consent.json"
        || name.starts_with('/')
        || name.ends_with('/')
        || name.contains('\\')
        || name.contains("..")
        || name
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
    {
        return Err(format!("包内路径不安全: {name:?}"));
    }
    Ok(())
}

/// 名字是否走二进制块(其余后缀必须 UTF-8 文本)。
pub fn is_binary(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| BINARY_EXTS.contains(&ext))
}

/// 把文件集编码成包字节流;名字、重名与文本编码在写入前校验。
pub fn encode(files: &Files) -> Result<Vec<u8>, String> {
    if files.is_empty() {
        return Err("扩展包至少要有一个文件".into());
    }
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    out.extend_from_slice(PACK_HEADER.as_bytes());
    out.push(b'\n');
    for (name, bytes) in files {
        check_name(name)?;
        if seen.contains(&name.as_str()) {
            return Err(format!("包内文件重名: {name}"));
        }
        seen.push(name);
        if is_binary(name) {
            out.extend_from_slice(format!("### binary: {name} {}\n", bytes.len()).as_bytes());
            out.extend_from_slice(bytes);
            out.push(b'\n');
            continue;
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| format!("{name} 不是 UTF-8 文本,不进扩展包"))?;
        out.extend_from_slice(format!("### begin: {name} {}\n", text.lines().count()).as_bytes());
        // 逐行写出:中间的空行原样保留;结尾补齐一个换行(文本文件规范)。
        for line in text.lines() {
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
        out.extend_from_slice(format!("### end: {name}\n").as_bytes());
    }
    Ok(out)
}

/// 解包字节流:文本块 `### begin: <路径> <行数>` … `### end:`,二进制块
/// `### binary: <路径> <字节数>` 后跟原文。清单解析留给调用方。
pub fn decode(data: &[u8]) -> Result<Files, String> {
    let bad = |why: String| format!("不是有效的扩展包: {why}");
    let header = format!("{PACK_HEADER}\n");
    if !data.starts_with(header.as_bytes()) {
        return Err("不是掠影扩展包(缺 lueying-pack 头)".into());
    }
    let mut blocks = Blocks::new(data);
    blocks.pos = header.len();
    let mut files: Files = Vec::new();
    while !blocks.at_end() {
        let line = blocks.line().map_err(bad)?;
        if let Some(rest) = line.strip_prefix("### begin: ") {
            let (name, count) = rest
                .rsplit_once(' ')
                .ok_or_else(|| bad(format!("begin 行缺少行数: {rest:?}")))?;
            check_name(name).map_err(bad)?;
            let count: usize = count
                .parse()
                .map_err(|_| bad(format!("begin 行行数不合法: {count:?}")))?;
            let mut content = Vec::new();
            for _ in 0..count {
                let body = blocks.line().map_err(bad)?;
                content.extend_from_slice(body.as_bytes());
                content.push(b'\n');
            }
            let end = blocks.line().map_err(bad)?;
            if end != format!("### end: {name}") {
                return Err(bad(format!("块 {name:?} 的 end 行对不上: {end:?}")));
            }
            push_unique(&mut files, name.to_string(), content).map_err(bad)?;
        } else if let Some(rest) = line.strip_prefix("### binary: ") {
            let (name, count) = rest
                .rsplit_once(' ')
                .ok_or_else(|| bad(format!("binary 行缺少字节数: {rest:?}")))?;
            check_name(name).map_err(bad)?;
            let count: usize = count
                .parse()
                .map_err(|_| bad(format!("binary 行字节数不合法: {count:?}")))?;
            let content = blocks.take(count).map_err(bad)?.to_vec();
            if blocks.take(1).map_err(bad)? != b"\n" {
                return Err(bad(format!("字节块 {name:?} 后缺换行")));
            }
            push_unique(&mut files, name.to_string(), content).map_err(bad)?;
        } else {
            return Err(bad(format!("包里出现未知行,文件可能被截断: {line:?}")));
        }
    }
    if files.is_empty() {
        return Err(bad("包是空的".into()));
    }
    Ok(files)
}

fn push_unique(files: &mut Files, name: String, content: Vec<u8>) -> Result<(), String> {
    if files.iter().any(|(other, _)| *other == name) {
        return Err(format!("包内文件重名: {name}"));
    }
    files.push((name, content));
    Ok(())
}

/// 包内容扫描器:在字节流上按块读,文本块与二进制块共用一个游标。
struct Blocks<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Blocks<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// 读一行(不含换行);必须是 UTF-8。
    fn line(&mut self) -> Result<&'a str, String> {
        let Some(end) = self.data[self.pos..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| self.pos + offset)
        else {
            return Err("包被截断:缺一行".into());
        };
        let line = std::str::from_utf8(&self.data[self.pos..end])
            .map_err(|_| "包里有非 UTF-8 的控制行".to_string())?;
        self.pos = end + 1;
        Ok(line)
    }

    /// 读定长字节。
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self.pos + count;
        if end > self.data.len() {
            return Err("包被截断:字节块不够长".into());
        }
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }
}

#[cfg(test)]
#[path = "container_tests.rs"]
mod tests;
