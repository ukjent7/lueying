//! 配置备份包:本机配置与词库打成一份文本,设置窗「备份」导出与导入共用同一格式。
//!
//! 每块按行数声明长度,块内出现什么标记都不影响解析;解析时先认文件头。

/// 载荷的文件头;解析时先认它。
pub const HEADER: &str = "### lueying-backup v1";

/// 打包:每块按行数声明长度,文件里出现什么标记都不影响解析。
pub fn pack(blocks: &[(String, String)]) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for (name, content) in blocks {
        let lines = content.lines().count();
        out.push_str(&format!("### begin: {name} {lines}\n"));
        if lines > 0 {
            out.push_str(content.trim_end_matches('\n'));
            out.push('\n');
        }
        out.push_str(&format!("### end: {name}\n"));
    }
    out
}

/// 解包:`### begin: <名字> <行数>` 之后逐行取声明数量的行,随后跳过
/// `### end:` 行;坏块整块跳过。
pub fn unpack(text: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("### begin: ") else {
            continue;
        };
        let Some((name, count)) = rest.rsplit_once(' ') else {
            continue;
        };
        let Ok(count) = count.parse::<usize>() else {
            continue;
        };
        let mut content = Vec::with_capacity(count);
        for _ in 0..count {
            match lines.next() {
                Some(line) => content.push(line.to_string()),
                None => break,
            }
        }
        lines.next_if(|line| line.starts_with("### end:"));
        files.push((name.to_string(), content.join("\n")));
    }
    files
}

/// 是否是本格式的载荷。
pub fn is_bundle(text: &str) -> bool {
    text.starts_with(HEADER)
}

#[cfg(test)]
#[path = "bundle_tests.rs"]
mod tests;
