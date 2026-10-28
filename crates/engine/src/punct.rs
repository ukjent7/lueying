//! 中文标点映射。成对引号按「下次输入交替开/闭」处理。

use std::collections::BTreeMap;

/// 可打印 ASCII 的全角形式:空格是 U+3000,其余落在 U+FF01–U+FF5E。
pub fn fullwidth(c: char) -> Option<char> {
    match c {
        ' ' => Some('\u{3000}'),
        '!'..='~' => char::from_u32(c as u32 - 0x21 + 0xff01),
        _ => None,
    }
}

/// 把 ASCII 标点映射为中文标点;不在表内的返回 `None`(按键直通)。
/// `dq_open` / `sq_open` 分别是双引号、单引号的开合状态。
/// `after_digit` 记录刚刚输入的是 ASCII 数字:此时 `.` 保持半角,
/// 让 `3.14`、`192.168.1.1` 这类内容不必切英文。
pub fn map(
    c: char,
    dq_open: &mut bool,
    sq_open: &mut bool,
    after_digit: &mut bool,
) -> Option<&'static str> {
    let digit_before = std::mem::replace(after_digit, false);
    Some(match c {
        ',' => "，",
        '.' if digit_before => return None,
        '.' => "。",
        '!' => "！",
        '?' => "？",
        ';' => "；",
        ':' => "：",
        '(' => "（",
        ')' => "）",
        '<' => "《",
        '>' => "》",
        '[' => "【",
        ']' => "】",
        '{' => "「",
        '}' => "」",
        '\\' => "、",
        '`' => "·",
        '~' => "～",
        '|' => "｜",
        // 与微软拼音一致:省略号、破折号各按一次出双字符。
        '^' => "……",
        '_' => "——",
        '$' => "￥",
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

/// 先查用户自定义表,再走默认映射;查不到返回 `None`(按键直通)。
/// 自定义项固定输出:引号交替与「数字后 `.` 保持半角」只属于默认映射,
/// 用户显式写了映射就按映射来,行为可预期。
pub fn map_with(
    custom: &BTreeMap<char, String>,
    c: char,
    dq_open: &mut bool,
    sq_open: &mut bool,
    after_digit: &mut bool,
) -> Option<String> {
    if let Some(text) = custom.get(&c) {
        std::mem::replace(after_digit, false);
        return Some(text.clone());
    }
    map(c, dq_open, sq_open, after_digit).map(str::to_string)
}
