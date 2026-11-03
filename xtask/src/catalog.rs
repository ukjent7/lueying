//! 发版时把原生引擎 exe 的下载地址与哈希补进官方目录。
//!
//! 目录在编译期嵌入 server,补丁必须发生在构建安装器之前;仓库里的
//! 条目是占位(全零哈希、零字节),占位条目装不上,所以每次发版都要
//! 跑这一步。
//!
//! `main` 的 `#[path]` 子模块(src 保持扁平)。

use std::path::PathBuf;

pub(crate) struct CatalogExeArgs {
    id: String,
    exe: PathBuf,
    url: String,
    catalog: PathBuf,
}

pub(crate) fn parse_catalog_exe(it: &mut impl Iterator<Item = String>) -> CatalogExeArgs {
    let mut args = CatalogExeArgs {
        id: "ppocr".to_string(),
        exe: PathBuf::from("target/release/ppocr.exe"),
        url: String::new(),
        catalog: PathBuf::from("Catalog/official-extensions.json"),
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--id" => args.id = it.next().unwrap_or(args.id),
            "--exe" => args.exe = it.next().map(PathBuf::from).unwrap_or(args.exe),
            "--url" => args.url = it.next().unwrap_or(args.url),
            "--catalog" => args.catalog = it.next().map(PathBuf::from).unwrap_or(args.catalog),
            _ => {}
        }
    }
    args
}

pub(crate) fn catalog_exe(args: CatalogExeArgs) -> Result<(), String> {
    if args.url.is_empty() {
        return Err("--url 需要发布资产的下载地址".into());
    }
    let name = args
        .exe
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("--exe 缺文件名")?
        .to_string();
    let file =
        std::fs::File::open(&args.exe).map_err(|e| format!("读 {}: {e}", args.exe.display()))?;
    let sha256 = lueying_extmeta::sha256::sha256_reader(file)
        .map_err(|e| format!("算 {}: {e}", args.exe.display()))?;
    let bytes = std::fs::metadata(&args.exe)
        .map_err(|e| format!("读 {}: {e}", args.exe.display()))?
        .len();
    let text = std::fs::read_to_string(&args.catalog)
        .map_err(|e| format!("读 {}: {e}", args.catalog.display()))?;
    let mut catalog: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("目录不是合法 JSON: {e}"))?;
    let Some(entry) = catalog["extensions"]
        .as_array_mut()
        .and_then(|list| list.iter_mut().find(|item| item["id"].as_str() == Some(args.id.as_str())))
    else {
        return Err(format!("目录里没有 {} 条目", args.id));
    };
    let files = entry["files"]
        .as_array_mut()
        .ok_or_else(|| format!("{} 条目没有 files", args.id))?;
    let exe_entry = files
        .iter_mut()
        .find(|file| file["name"].as_str() == Some(name.as_str()))
        .ok_or_else(|| format!("{} 条目缺 {name} 文件", args.id))?;
    exe_entry["url"] = serde_json::Value::String(args.url);
    exe_entry["sha256"] = serde_json::Value::String(sha256.clone());
    exe_entry["bytes"] = serde_json::Value::Number(bytes.into());
    let pretty = serde_json::to_string_pretty(&catalog).map_err(|e| e.to_string())?;
    std::fs::write(&args.catalog, pretty + "\n")
        .map_err(|e| format!("写 {}: {e}", args.catalog.display()))?;
    println!("目录已更新:{name} {bytes} 字节,sha256 {sha256}");
    Ok(())
}
