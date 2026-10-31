//! 错音纠正:整串输入没有任何整句候选时,按一次编辑距离找最像的按键串。
//!
//! 只处理全小写字母串,顺序是相邻字母调换、相邻键替换、漏键插入、多余键删除;
//! 多条纠正串都能出整句时,由调用方取打分最高的一条,不先到先得。纠正候选按
//! 整串上屏,学习键由纠正后的音节串单独保存,不能从原串的字节区间反推。

/// 纠正只在这段长度内尝试,过长的串通常不是手误。
const MAX_TYPO_LEN: usize = 24;
/// 一次纠正最多生成多少条候选串,防止极端长串把首键延迟拖长。
const MAX_CORRECTIONS: usize = 400;

const KEY_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

/// 是否值得尝试纠正:至少两键,且全为小写字母。
pub(super) fn eligible(raw: &str) -> bool {
    (2..=MAX_TYPO_LEN).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_lowercase())
}

/// 按「相邻字母调换 → 相邻键替换 → 漏键插入 → 多余键删除」生成纠正串。
pub(super) fn variants(raw: &str) -> Vec<String> {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();

    // 调换相邻字母:快速连打时常见,只改一个位置,精度高于补键/删键。
    for i in 0..len.saturating_sub(1) {
        if bytes[i] == bytes[i + 1] {
            continue;
        }
        let mut text = String::with_capacity(len);
        text.push_str(&raw[..i]);
        text.push(bytes[i + 1] as char);
        text.push(bytes[i] as char);
        text.push_str(&raw[i + 2..]);
        out.push(text);
        if out.len() == MAX_CORRECTIONS {
            return out;
        }
    }

    for (i, &byte) in bytes.iter().enumerate() {
        for replacement in neighbors(byte as char) {
            let mut text = String::with_capacity(len);
            text.push_str(&raw[..i]);
            text.push(replacement);
            text.push_str(&raw[i + 1..]);
            out.push(text);
            if out.len() == MAX_CORRECTIONS {
                return out;
            }
        }
    }

    for i in 0..=len {
        for inserted in 'a'..='z' {
            let mut text = String::with_capacity(len + 1);
            text.push_str(&raw[..i]);
            text.push(inserted);
            text.push_str(&raw[i..]);
            out.push(text);
            if out.len() == MAX_CORRECTIONS {
                return out;
            }
        }
    }

    for i in 0..len {
        let mut text = String::with_capacity(len - 1);
        text.push_str(&raw[..i]);
        text.push_str(&raw[i + 1..]);
        out.push(text);
        if out.len() == MAX_CORRECTIONS {
            return out;
        }
    }
    out
}

/// 键盘上与该键相邻的字母,同排左右与上下两排的错位键。
fn neighbors(c: char) -> Vec<char> {
    let Some((row, col)) = position(c) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (other_row, keys) in KEY_ROWS.iter().enumerate() {
        if other_row.abs_diff(row) > 1 {
            continue;
        }
        for (other_col, key) in keys.chars().enumerate() {
            if key != c && (x(other_row, other_col) - x(row, col)).abs() <= 1.0 {
                out.push(key);
            }
        }
    }
    out
}

fn position(c: char) -> Option<(usize, usize)> {
    KEY_ROWS
        .iter()
        .enumerate()
        .find_map(|(row, keys)| keys.chars().position(|key| key == c).map(|col| (row, col)))
}

/// 三排键盘的相对横向坐标,让上下排的键位错开半格。
fn x(row: usize, col: usize) -> f32 {
    col as f32 + row as f32 * 0.5
}
