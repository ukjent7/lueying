//! 错音与音素混淆度纠正:基于键盘物理欧氏距离与发音音素两级查找表的软惩罚模型。
//!
//! 替代死板的离散编辑距离与等同规则,为相邻按键调换、物理近邻替换、平翘舌/前后鼻音
//! 音素混淆及漏键/赘键赋予连续软惩罚(Soft-Penalty),使纠错候选自然融入语言模型打分。

/// 纠正只在这段长度内尝试,过长的串通常不是手误。
const MAX_TYPO_LEN: usize = 24;
/// 一次纠正最多生成多少条候选串,防止极端长串把首键延迟拖长。
const MAX_CORRECTIONS: usize = 400;

const KEY_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

/// 纠正变体及对应软惩罚分。
#[derive(Clone, Debug)]
pub struct Correction {
    pub text: String,
    pub penalty: f64,
}

/// 是否值得尝试纠正:至少两键,且全为小写字母。
pub(super) fn eligible(raw: &str) -> bool {
    (2..=MAX_TYPO_LEN).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_lowercase())
}

/// 按「相邻字母调换 → 音素/物理键替换 → 漏键插入 → 多余键删除」生成带软惩罚的纠正串。
pub(super) fn corrections(raw: &str) -> Vec<Correction> {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();

    // 1. 调换相邻字母:快速连打手误常见,物理相邻字母调换惩罚 0.40
    for i in 0..len.saturating_sub(1) {
        if bytes[i] == bytes[i + 1] {
            continue;
        }
        let mut text = String::with_capacity(len);
        text.push_str(&raw[..i]);
        text.push(bytes[i + 1] as char);
        text.push(bytes[i] as char);
        text.push_str(&raw[i + 2..]);
        out.push(Correction {
            text,
            penalty: 0.40,
        });
        if out.len() == MAX_CORRECTIONS {
            return out;
        }
    }

    // 2. 音素与物理键位两级混淆替换
    for (i, &byte) in bytes.iter().enumerate() {
        let ch = byte as char;
        for (replacement, penalty) in confused_replacements(ch) {
            let mut text = String::with_capacity(len);
            text.push_str(&raw[..i]);
            text.push(replacement);
            text.push_str(&raw[i + 1..]);
            out.push(Correction { text, penalty });
            if out.len() == MAX_CORRECTIONS {
                return out;
            }
        }
    }

    // 3. 漏键插入(含前后鼻音补 g、平翘舌补 h 特殊软惩罚)
    for i in 0..=len {
        let prev = if i > 0 {
            Some(bytes[i - 1] as char)
        } else {
            None
        };
        for inserted in 'a'..='z' {
            let penalty = match (prev, inserted) {
                // n 后面漏打 g (前后鼻音: an -> ang, in -> ing, en -> eng) 仅罚 0.25
                (Some('n'), 'g') => 0.25,
                // z/c/s 后面漏打 h (平翘舌: z -> zh, c -> ch, s -> sh) 仅罚 0.25
                (Some('z' | 'c' | 's'), 'h') => 0.25,
                _ => 0.85,
            };
            let mut text = String::with_capacity(len + 1);
            text.push_str(&raw[..i]);
            text.push(inserted);
            text.push_str(&raw[i..]);
            out.push(Correction { text, penalty });
            if out.len() == MAX_CORRECTIONS {
                return out;
            }
        }
    }

    // 4. 多余键删除(含赘 g、赘 h 特殊软惩罚)
    for i in 0..len {
        let ch = bytes[i] as char;
        let prev = if i > 0 {
            Some(bytes[i - 1] as char)
        } else {
            None
        };
        let penalty = match (prev, ch) {
            (Some('n'), 'g') => 0.25,
            (Some('z' | 'c' | 's'), 'h') => 0.25,
            _ => 0.85,
        };
        let mut text = String::with_capacity(len - 1);
        text.push_str(&raw[..i]);
        text.push_str(&raw[i + 1..]);
        out.push(Correction { text, penalty });
        if out.len() == MAX_CORRECTIONS {
            return out;
        }
    }

    // 按软惩罚从小到大排序,优先探索高概率纠错候选
    out.sort_by(|a, b| a.penalty.total_cmp(&b.penalty));
    out
}

/// 一个窗口的单处手误变体及罚分,供音节图使用。罚分与 [`corrections`] 同源:
/// 相邻字母交换 0.40,替换按音素与物理键表,多打或漏打一个字母 0.85(`n→ng`、`z/c/s→h` 为 0.25)。
pub(crate) fn variants(window: &str) -> Vec<(String, f64)> {
    let bytes = window.as_bytes();
    let len = bytes.len();
    let mut out = Vec::new();
    for i in 0..len.saturating_sub(1) {
        if bytes[i] != bytes[i + 1] {
            let mut text = window.to_string();
            text.replace_range(i..i + 2, &format!("{}{}", bytes[i + 1] as char, bytes[i] as char));
            out.push((text, 0.40));
        }
    }
    for (i, &byte) in bytes.iter().enumerate() {
        for (replacement, penalty) in confused_replacements(byte as char) {
            let mut text = window.to_string();
            text.replace_range(i..i + 1, &replacement.to_string());
            out.push((text, penalty));
        }
    }
    for i in 0..=len {
        let prev = (i > 0).then(|| bytes[i - 1] as char);
        for inserted in 'a'..='z' {
            let mut text = window.to_string();
            text.insert(i, inserted);
            out.push((text, extra_letter_penalty(prev, inserted)));
        }
    }
    for i in 0..len {
        let prev = (i > 0).then(|| bytes[i - 1] as char);
        let mut text = window.to_string();
        text.remove(i);
        out.push((text, extra_letter_penalty(prev, bytes[i] as char)));
    }
    out
}

/// 多打或漏打一个字母的罚分:鼻音 `n`→`g`、平翘 `z/c/s`→`h` 较常见,罚得轻。
fn extra_letter_penalty(prev: Option<char>, letter: char) -> f64 {
    match (prev, letter) {
        (Some('n'), 'g') | (Some('z' | 'c' | 's'), 'h') => 0.25,
        _ => 0.85,
    }
}

/// 某键的音素混淆与物理近邻替换候选及对应的软惩罚分。
fn confused_replacements(c: char) -> Vec<(char, f64)> {
    let mut out = Vec::new();

    // 1. 发音音素混淆表 (前后鼻音、平翘舌、易混辅音)
    let phonetic_pairs: &[(char, f64)] = match c {
        'n' => &[('l', 0.25)],
        'l' => &[('n', 0.25), ('r', 0.35)],
        'f' => &[('h', 0.30)],
        'h' => &[('f', 0.30)],
        'r' => &[('l', 0.35)],
        'z' => &[('c', 0.35), ('s', 0.40)],
        'c' => &[('z', 0.35), ('s', 0.35)],
        's' => &[('z', 0.40), ('c', 0.35), ('x', 0.40)],
        'b' => &[('p', 0.40)],
        'p' => &[('b', 0.40)],
        'd' => &[('t', 0.40)],
        't' => &[('d', 0.40)],
        'g' => &[('k', 0.40)],
        'k' => &[('g', 0.40)],
        'j' => &[('q', 0.45)],
        'q' => &[('j', 0.45)],
        _ => &[],
    };

    for &(replacement, penalty) in phonetic_pairs {
        out.push((replacement, penalty));
    }

    // 2. 键盘物理欧氏距离近邻
    let Some((row, col)) = position(c) else {
        return out;
    };
    let c_pos = (x(row, col), y(row));

    for (other_row, keys) in KEY_ROWS.iter().enumerate() {
        if other_row.abs_diff(row) > 1 {
            continue;
        }
        for (other_col, key) in keys.chars().enumerate() {
            if key == c {
                continue;
            }
            let key_pos = (x(other_row, other_col), y(other_row));
            let dx = c_pos.0 - key_pos.0;
            let dy = c_pos.1 - key_pos.1;
            let dist = (dx * dx + dy * dy).sqrt() as f64;
            if dist <= 1.15 {
                // 物理相邻按键: 0.60 ~ 0.65
                let phys_penalty = 0.60 + 0.30 * (dist - 1.0).max(0.0);
                if let Some(existing) = out.iter_mut().find(|(r, _)| *r == key) {
                    if phys_penalty < existing.1 {
                        existing.1 = phys_penalty;
                    }
                } else {
                    out.push((key, phys_penalty));
                }
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

/// 三排键盘横向相对坐标,让上下排键位错开半格。
fn x(row: usize, col: usize) -> f32 {
    col as f32 + row as f32 * 0.5
}

/// 纵向坐标。
fn y(row: usize) -> f32 {
    row as f32
}
