//! `lueying-settings ext <子命令>`:终端装扩展的入口,与设置窗共用同一条落盘路径。
//!
//! 只做 `install`(本地 `.luext` 包)与 `add`(GitHub 来源);开发期的 new/pack
//! 在 `lueying` 开发者工具里。窗口子系统的进程输出走调用方控制台,见 `main.rs`。

use std::path::Path;

use lueying_server::config;

use crate::ext_add;
use crate::ext_install::{MAX_PACKAGE_BYTES, install_luext_bytes};

const USAGE: &str = "\
用法:
  lueying-settings ext install <扩展包.luext>
  lueying-settings ext add github:作者/仓库#版本";

/// 执行 `ext` 子命令(`args` 不含 `ext`);返回给人看的汇报。
pub fn run(args: &[String]) -> Result<String, String> {
    match args {
        [sub, path] if sub == "install" => install(Path::new(path)),
        [sub, source] if sub == "add" => ext_add::add(source),
        _ => Err(USAGE.to_string()),
    }
}

fn install(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path)
        .map_err(|e| format!("读 {}: {e}", path.display()))?
        .len();
    if size > MAX_PACKAGE_BYTES {
        return Err("扩展包超过 128 MiB 上限".into());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("读 {}: {e}", path.display()))?;
    let root = config::extensions_dir().ok_or("找不到扩展目录(APPDATA 或 LUEYING_CONFIG)")?;
    install_luext_bytes(&bytes, &root)
}
