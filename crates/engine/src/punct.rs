//! 中文标点映射。成对引号按「下次输入交替开/闭」处理。

/// 把 ASCII 标点映射为中文标点;不在表内的返回 `None`(按键直通)。
/// `dq_open` / `sq_open` 分别是双引号、单引号的开合状态。
pub fn map(c: char, dq_open: &mut bool, sq_open: &mut bool) -> Option<&'static str> {
    Some(match c {
        ',' => "，",
        '.' => "。",
        '!' => "！",
        '?' => "？",
        ';' => "；",
        ':' => "：",
        '(' => "（",
        ')' => "）",
        '<' => "〈",
        '>' => "〉",
        '\\' => "、",
        '"' => {
            let r = if *dq_open { "”" } else { "“" };
            *dq_open = !*dq_open;
            r
        }
        '\'' => {
            let r = if *sq_open { "’" } else { "‘" };
            *sq_open = !*sq_open;
            r
        }
        _ => return None,
    })
}
