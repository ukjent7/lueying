//! 打字记录转评测语料:`lueying-bench import-typing <记录目录> [--out <文件>]`。
//!
//! 合并记录目录里的 `typing-log.tsv`(干净句)与 `typing-edits.tsv`(编辑过的句子)。
//! 键入串是原样击键,期望是最终上屏的文字;编辑标记(`category = edit-live`)与末列 `trail` 原样保留。
//! 格式见 `lueying-core` 的 `typing_log` 模块。输出名须以 `eval-user-` 开头,
//! 这样 CI 把它当私人语料处理:逐条结果只进加密文件。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const HEADER: &str = "# 打字记录导入的用户语料(仅评测,不上传)。列: category<TAB>source<TAB>pinyin<TAB>expected<TAB>context(空)[<TAB>trail]\n";

fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|at| args.get(at + 1))
}

/// 返回进程退出码。
pub fn run(args: &[String]) -> i32 {
    let Some(dir) = args.get(2) else {
        eprintln!("用法: lueying-bench import-typing <记录目录> [--out <文件>]");
        return 2;
    };
    let out = value(args, "--out").map_or_else(
        || Path::new(dir).join("eval-user-typing.tsv"),
        PathBuf::from,
    );
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !crate::shared::is_sensitive(&name) {
        eprintln!("输出文件名须以 eval-user- 开头(私人语料,CI 只输出汇总)");
        return 2;
    }
    match convert(Path::new(dir)) {
        Ok((body, count)) => match std::fs::write(&out, body) {
            Ok(()) => {
                println!("写出 {}: {count} 条", out.display());
                0
            }
            Err(error) => {
                eprintln!("写 {} 失败: {error}", out.display());
                1
            }
        },
        Err(error) => {
            eprintln!("读取记录失败: {error}");
            1
        }
    }
}

/// 两份记录合成一份语料正文;没有记录文件时只有表头。
fn convert(dir: &Path) -> std::io::Result<(String, usize)> {
    let mut body = String::from(HEADER);
    let mut count = 0;
    for file in ["typing-log.tsv", "typing-edits.tsv"] {
        let path = dir.join(file);
        if !path.exists() {
            continue;
        }
        for line in std::fs::read_to_string(path)?.lines() {
            let cols: Vec<&str> = line.split('\t').collect();
            // 注释、空行与残缺行跳过;期望为空的句子没有可比对的答案。
            if line.starts_with('#') || cols.len() < 4 || cols[2].is_empty() || cols[3].is_empty() {
                continue;
            }
            let _ = writeln!(body, "{line}");
            count += 1;
        }
    }
    Ok((body, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_clean_and_edit_rows_and_keeps_markers() {
        let dir =
            std::env::temp_dir().join(format!("lueying-import-typing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("typing-log.tsv"),
            "# 头\nuser-live\tuser\tnihao\t你好\t\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("typing-edits.tsv"),
            "# 头\nedit-live\tuser\tnihhao\t你好\t\tnihh#ao@\n",
        )
        .unwrap();
        let (body, count) = convert(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(count, 2);
        assert!(body.contains("user-live\tuser\tnihao\t你好\t\n"));
        assert!(body.contains("edit-live\tuser\tnihhao\t你好\t\tnihh#ao@\n"));
    }
}
