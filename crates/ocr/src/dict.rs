//! rec 的词表:一行一个字,顺序与模型输出通道一一对应。
//!
//! 模型输出 18710 类 = 18708 个词表字 + CTC 空白 + 空格;空白固定是 0,
//! 空格固定是最后一类。词表不合法(空文件或含空行)时明确报错,避免把
//! 错位文本静默上屏。

use std::path::Path;

pub(crate) fn load(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    let mut dict: Vec<String> = text.lines().map(str::to_string).collect();
    if dict.last().is_some_and(|line| line.is_empty()) {
        dict.pop();
    }
    if dict.is_empty() {
        return Err(format!("{} 是空词表", path.display()));
    }
    if dict.iter().any(|line| line.is_empty()) {
        return Err(format!("{} 含空行", path.display()));
    }
    Ok(dict)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "lueying-ocr-dict-{}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn keeps_order_and_trailing_newline() {
        let path = temp("ok");
        std::fs::write(&path, "a\n中\n\n").unwrap();
        let dict = load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(dict, vec!["a", "中"]);
    }

    #[test]
    fn rejects_empty_and_blank_lines() {
        let empty = temp("empty");
        std::fs::write(&empty, "\n").unwrap();
        assert!(load(&empty).is_err());
        let _ = std::fs::remove_file(&empty);
        let blank = temp("blank");
        std::fs::write(&blank, "a\n\nb\n").unwrap();
        assert!(load(&blank).is_err());
        let _ = std::fs::remove_file(&blank);
    }
}
