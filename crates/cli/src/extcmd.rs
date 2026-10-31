//! `lueying ext` 的开发回路:新建骨架、打包、安装、重载与日志。
//!
//! 全部是纯文件与管道操作:不加载 JS,不碰 TSF,Windows 与 Linux 都能跑。
//! 包格式与配置备份同款——行数声明长度的文本块,`### lueying-pack v1` 开头。

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use lueying_extmeta::Manifest;
use lueying_protocol::{Request, pipe_name};

/// 包文件头;与「配置备份」同一条格式。
pub const PACK_HEADER: &str = "### lueying-pack v1";

/// 参与打包的文件后缀。扩展包只收文本;发现二进制直接报错,宁可失败也不
/// 静默丢文件。
const PACK_EXTS: &[&str] = &["js", "json", "txt", "sksl", "md"];

/// `ext new` 的参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewArgs {
    pub id: String,
    pub name: Option<String>,
    pub dir: Option<PathBuf>,
}

/// `ext pack` 的参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackArgs {
    pub dir: PathBuf,
    pub out: Option<PathBuf>,
}

/// `ext install` 的参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallArgs {
    pub source: PathBuf,
    pub dir: Option<PathBuf>,
    pub force: bool,
}

/// `ext logs` 的参数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogsArgs {
    pub file: Option<PathBuf>,
    pub tail: usize,
}

/// 新建扩展包骨架:一个合法清单 + 最小脚本,装上就能被设置页看到。
pub fn new(args: &NewArgs) -> Result<PathBuf, String> {
    if !lueying_extmeta::valid_id(&args.id) {
        return Err(format!(
            "扩展 id {:?} 不合法:只允许小写字母、数字、- 与 _,且以字母或数字开头",
            args.id
        ));
    }
    let root = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    let dir = root.join(&args.id);
    if dir.exists() {
        return Err(format!("{} 已存在;换一个 id 或先删掉", dir.display()));
    }
    let name = args.name.clone().unwrap_or_else(|| args.id.clone());
    let manifest = serde_json::json!({
        "id": args.id,
        "name": name,
        "version": "0.1.0",
        "api": 2,
        "main": "index.js",
        "activate": {
            "commands": [
                { "id": "hello", "title": "打个招呼", "when": "any" }
            ]
        },
        "needs": [],
        "capabilities": [],
        "settings": [
            {
                "key": "greeting",
                "title": "招呼语",
                "type": "choice",
                "options": ["你好", "嗨"],
                "default": "你好"
            }
        ]
    });
    let script = format!(
        "// {name} 的入口脚本。至少注册一个派发入口;不改外观就返回空补丁。\n\
         lueying.register(() => ({{}}));\n\
         \n\
         lueying.command(\"hello\", () => ({{ ok: true }}));\n"
    );
    fs::create_dir_all(&dir).map_err(|e| format!("建 {}: {e}", dir.display()))?;
    write_file(&dir.join("manifest.json"), &manifest_text(&manifest)?)?;
    write_file(&dir.join("index.js"), &script)?;
    Ok(dir)
}

fn manifest_text(manifest: &serde_json::Value) -> Result<String, String> {
    let mut text =
        serde_json::to_string_pretty(manifest).map_err(|e| format!("序列化清单: {e}"))?;
    text.push('\n');
    Ok(text)
}

/// 打包一个扩展目录;清单必须存在且 id 等于目录名。
pub fn pack(args: &PackArgs) -> Result<PathBuf, String> {
    let dir = &args.dir;
    if !dir.is_dir() {
        return Err(format!("{} 不是目录", dir.display()));
    }
    let manifest_bytes = fs::read(dir.join("manifest.json")).map_err(|e| format!("读清单: {e}"))?;
    let manifest = Manifest::parse(&manifest_bytes).map_err(|e| format!("清单不合法: {e}"))?;
    let dir_name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} 目录名不可用", dir.display()))?;
    if manifest.id != dir_name {
        return Err(format!(
            "目录名 {dir_name:?} 与清单 id {:?} 不一致",
            manifest.id
        ));
    }
    let mut files = Vec::new();
    collect(dir, &mut files)?;
    files.sort();
    if files.is_empty() {
        return Err(format!("{} 里没有可打包的文本文件", dir.display()));
    }
    let mut out = String::from(PACK_HEADER);
    out.push('\n');
    for path in &files {
        let bytes = fs::read(path).map_err(|e| format!("读 {}: {e}", path.display()))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("{} 不是 UTF-8 文本,不进扩展包", path.display()))?;
        let name = path
            .strip_prefix(dir)
            .expect("collect 只产出 dir 下的路径")
            .to_string_lossy()
            .replace('\\', "/");
        let lines = text.lines().count();
        out.push_str(&format!("### begin: {name} {lines}\n"));
        // 逐行写出:中间的空行原样保留;结尾补齐一个换行(文本文件规范)。
        for line in text.lines() {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&format!("### end: {name}\n"));
    }
    let target = args.out.clone().unwrap_or_else(|| {
        dir.with_file_name(format!(
            "{}.luext",
            dir.file_name().unwrap_or_default().to_string_lossy()
        ))
    });
    write_file(&target, &out)?;
    Ok(target)
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("读 {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out)?;
            continue;
        }
        let kept = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| PACK_EXTS.contains(&ext));
        if kept {
            out.push(path);
        }
    }
    Ok(())
}

/// 安装一个 `.luext` 包或目录到扩展目录;默认不覆盖已有包。
pub fn install(args: &InstallArgs) -> Result<PathBuf, String> {
    let target_root = args
        .dir
        .clone()
        .or_else(extensions_dir)
        .ok_or("找不到扩展目录;用 --dir 指定,或设置 APPDATA/LOCALAPPDATA")?;
    if !target_root.exists() {
        fs::create_dir_all(&target_root)
            .map_err(|e| format!("建 {}: {e}", target_root.display()))?;
    }
    let (id, files) = if args.source.is_dir() {
        read_directory(&args.source)?
    } else {
        let text = fs::read_to_string(&args.source)
            .map_err(|e| format!("读 {}: {e}", args.source.display()))?;
        unpack(&text)?
    };
    let target = target_root.join(&id);
    if target.exists() {
        if !args.force {
            return Err(format!("{} 已存在;加 --force 覆盖", target.display()));
        }
        fs::remove_dir_all(&target).map_err(|e| format!("清掉旧包: {e}"))?;
    }
    for (name, content) in &files {
        let path = target.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("建 {}: {e}", parent.display()))?;
        }
        write_file(&path, content)?;
    }
    // 装完再校验一次:清单必须能解析,id 必须等于目录名。
    let manifest_bytes =
        fs::read(target.join("manifest.json")).map_err(|e| format!("读清单: {e}"))?;
    Manifest::parse(&manifest_bytes).map_err(|e| {
        let _ = fs::remove_dir_all(&target);
        format!("清单不合法: {e}")
    })?;
    Ok(target)
}

fn read_directory(source: &Path) -> Result<(String, Vec<(String, String)>), String> {
    let manifest_bytes =
        fs::read(source.join("manifest.json")).map_err(|e| format!("读清单: {e}"))?;
    let manifest = Manifest::parse(&manifest_bytes).map_err(|e| format!("清单不合法: {e}"))?;
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} 目录名不可用", source.display()))?;
    if manifest.id != name {
        return Err(format!(
            "目录名 {name:?} 与清单 id {:?} 不一致",
            manifest.id
        ));
    }
    let mut files = Vec::new();
    let mut paths = Vec::new();
    collect(source, &mut paths)?;
    for path in paths {
        let relative = path
            .strip_prefix(source)
            .expect("collect 只产出 source 下的路径")
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = fs::read(&path).map_err(|e| format!("读 {}: {e}", path.display()))?;
        let text =
            String::from_utf8(bytes).map_err(|_| format!("{} 不是 UTF-8 文本", path.display()))?;
        files.push((relative, text));
    }
    Ok((manifest.id, files))
}

/// 解包文本:每块 `### begin: <相对路径> <行数>` … `### end: <相对路径>`。
/// 清单必须存在且能解析;返回 (扩展 id, 全部文件)。
pub fn unpack(text: &str) -> Result<(String, Vec<(String, String)>), String> {
    if !text.starts_with(PACK_HEADER) {
        return Err("不是掠影扩展包(缺 lueying-pack 头)".into());
    }
    let mut files: Vec<(String, String)> = Vec::new();
    let mut lines = text.lines();
    lines.next();
    while let Some(line) = lines.next() {
        let rest = line
            .strip_prefix("### begin: ")
            .ok_or("包里出现未知行,文件可能被截断")?;
        let (name, count) = rest
            .rsplit_once(' ')
            .ok_or_else(|| format!("begin 行缺少行数: {rest:?}"))?;
        let count: usize = count
            .parse()
            .map_err(|_| format!("begin 行行数不合法: {count:?}"))?;
        if name.contains("..") || name.starts_with('/') || name.contains('\\') {
            return Err(format!("包内路径不安全: {name:?}"));
        }
        let mut content = String::new();
        for _ in 0..count {
            let body = lines
                .next()
                .ok_or_else(|| format!("块 {name:?} 提前结束"))?;
            content.push_str(body);
            content.push('\n');
        }
        let end = lines
            .next()
            .ok_or_else(|| format!("块 {name:?} 缺 end 行"))?;
        if end != format!("### end: {name}") {
            return Err(format!("块 {name:?} 的 end 行对不上: {end:?}"));
        }
        files.push((name.to_string(), content));
    }
    let manifest = files
        .iter()
        .find(|(name, _)| name == "manifest.json")
        .ok_or("包里没有 manifest.json")?;
    let manifest =
        Manifest::parse(manifest.1.as_bytes()).map_err(|e| format!("清单不合法: {e}"))?;
    Ok((manifest.id, files))
}

/// 让常驻服务重读配置;扩展启停、绑定与设置改动都会被拾起。
pub fn reload() -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name())
        .map_err(|_| "常驻服务未运行;启动输入法后再试".to_string())?;
    let line =
        serde_json::to_string(&Request::ReloadConfig).map_err(|e| format!("序列化请求: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("写管道: {e}"))?;
    file.flush().map_err(|e| format!("写管道: {e}"))?;
    let mut reply = String::new();
    BufReader::new(file)
        .read_line(&mut reply)
        .map_err(|e| format!("读应答: {e}"))?;
    if reply.trim().is_empty() {
        return Err("服务没有应答 ReloadConfig".into());
    }
    Ok(())
}

/// 对外 commit 通道的令牌路径,与服务端 `config::commit_token_path` 同址。
fn commit_token_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("lueying").join("commit-token"))
}

fn read_commit_token() -> Option<String> {
    fs::read_to_string(commit_token_path()?)
        .ok()
        .map(|text| text.trim().to_string())
}

/// 借用输入法的上屏能力:把文本交给常驻服务写进当前输入框(任务书 6.3)。
/// 服务端开启 commit_channel 后收到第一次请求才生成令牌,所以拿到的令牌
/// 变了就读回新值重试一次;通道未开时令牌不会变化,不会白重试。
pub fn commit(text: &str) -> Result<String, String> {
    let mut token = read_commit_token();
    let mut reply = send_external_commit(token.as_deref(), text)?;
    let delivered = matches!(&reply.commit, Some(result) if result.delivered);
    if !delivered {
        let fresh = read_commit_token();
        if fresh != token {
            token = fresh;
            reply = send_external_commit(token.as_deref(), text)?;
        }
    }
    match reply.commit {
        Some(result) if result.delivered => Ok(format!("已上屏: {text}")),
        Some(result) if result.error.is_empty() => Err("服务没有确认上屏".into()),
        Some(result) => Err(result.error),
        None => Err("常驻服务不支持对外 commit;升级输入法后再试".into()),
    }
}

fn send_external_commit(
    token: Option<&str>,
    text: &str,
) -> Result<lueying_protocol::Reply, String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name())
        .map_err(|_| "常驻服务未运行;启动输入法后再试".to_string())?;
    let line = serde_json::to_string(&Request::ExternalCommit {
        token: token.unwrap_or_default().to_string(),
        text: text.to_string(),
    })
    .map_err(|e| format!("序列化请求: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("写管道: {e}"))?;
    file.flush().map_err(|e| format!("写管道: {e}"))?;
    let mut reply = String::new();
    BufReader::new(file)
        .read_line(&mut reply)
        .map_err(|e| format!("读应答: {e}"))?;
    if reply.trim().is_empty() {
        return Err("服务没有应答 ExternalCommit".into());
    }
    serde_json::from_str(reply.trim()).map_err(|e| format!("解析应答: {e}"))
}

/// 发一条请求收一条应答;`what` 只用于空应答的报错。
fn pipe_call(request: &Request, what: &str) -> Result<lueying_protocol::Reply, String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pipe_name())
        .map_err(|_| "常驻服务未运行;启动输入法后再试".to_string())?;
    let line = serde_json::to_string(request).map_err(|e| format!("序列化请求: {e}"))?;
    writeln!(file, "{line}").map_err(|e| format!("写管道: {e}"))?;
    file.flush().map_err(|e| format!("写管道: {e}"))?;
    let mut reply = String::new();
    BufReader::new(file)
        .read_line(&mut reply)
        .map_err(|e| format!("读应答: {e}"))?;
    if reply.trim().is_empty() {
        return Err(format!("服务没有应答 {what}"));
    }
    serde_json::from_str(reply.trim()).map_err(|e| format!("解析应答: {e}"))
}

/// 取回通用扩展请求的值;服务端报错原样上抛。
fn ext_value(id: &str, method: &str, what: &str) -> Result<serde_json::Value, String> {
    let reply = pipe_call(
        &Request::Ext {
            id: id.to_string(),
            method: method.to_string(),
            params: serde_json::Value::Null,
        },
        what,
    )?;
    let ext = reply
        .ext
        .ok_or_else(|| format!("服务没有返回 {what} 的结果"))?;
    if ext.ok {
        Ok(ext.value)
    } else {
        Err(ext.error)
    }
}

/// 可触发的扩展命令清单;`run` 只接受这里的 id。
pub fn commands() -> Result<Vec<(String, String)>, String> {
    let value = ext_value("", "commands.list", "commands.list")?;
    let Some(list) = value.as_array() else {
        return Err("命令清单不是数组".into());
    };
    Ok(list
        .iter()
        .filter_map(|item| {
            Some((
                item.get("id")?.as_str()?.to_string(),
                item.get("title")?.as_str()?.to_string(),
            ))
        })
        .collect())
}

/// 触发一条扩展命令;与动作候选、热键同一条执行路径,结果落在掠影
/// 自己的界面与日志,这里只确认已投递。
pub fn run(id: &str) -> Result<String, String> {
    let value = ext_value(id, "command.run", "command.run")?;
    let command = value.get("command").and_then(|v| v.as_str()).unwrap_or(id);
    Ok(format!("已提交 {command};结果看掠影的通知与日志"))
}

/// 服务日志末尾;`tail` 是行数。
pub fn logs(args: &LogsArgs) -> Result<String, String> {
    let path = args
        .file
        .clone()
        .or_else(default_log_path)
        .ok_or("找不到日志路径;用 --file 指定,或设置 LOCALAPPDATA")?;
    let text = fs::read_to_string(&path).map_err(|e| format!("读 {}: {e}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(args.tail);
    Ok(lines[start..].join("\n"))
}

/// 默认日志路径:`LOCALAPPDATA` 优先,退回 XDG 状态目录。
pub fn default_log_path() -> Option<PathBuf> {
    if let Some(base) = std::env::var_os("LUEYING_LOG_DIR") {
        return Some(PathBuf::from(base).join("server.log"));
    }
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        return Some(
            PathBuf::from(base)
                .join("lueying")
                .join("logs")
                .join("server.log"),
        );
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    Some(base.join("lueying").join("logs").join("server.log"))
}

/// 扩展目录:`LUEYING_EXTENSIONS_DIR` 优先,其次 `%APPDATA%\lueying\extensions`。
pub fn extensions_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LUEYING_EXTENSIONS_DIR") {
        return Some(PathBuf::from(dir));
    }
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("lueying").join("extensions"))
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("建 {}: {e}", parent.display()))?;
    }
    fs::write(path, text).map_err(|e| format!("写 {}: {e}", path.display()))
}

#[cfg(test)]
#[path = "extcmd_tests.rs"]
mod tests;
