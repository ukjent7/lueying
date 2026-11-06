//! 输入信道:把击键串读成带代价的音节单元。
//!
//! 全拼、简拼、模糊音、手误都是「用户想打某个音节,键盘上出现了这串字母」的不同读法,
//! 差别只在这种读法有多意外。这里把它们放进同一张图:`edges[i]` 是从字节位置 `i` 出发的
//! 全部单元,`cost` 是对数概率(nat)的负值。解码只跑一次,语言分减信道代价决定名次,
//! 不再有「先精确、失败了再纠错」的分支。隔音符只作硬边界,不产单元。

use std::collections::HashMap;
use std::sync::OnceLock;

use lueying_dict::syllables::{
    ABBREV_DIGRAPHS, ABBREV_LETTERS, Fuzzy, SYLLABLES, canonical, expansions_scored,
    initial_alternatives_scored, is_syllable,
};

/// 每处模糊改动的代价,等同词频减半;与青简、清风及 librime/libime 同轴。
const FUZZY_COST: f64 = std::f64::consts::LN_2;
/// 整串能按全拼读完时,混入简拼单元的代价。`hen` 切成 `he` + `n` 能把「很亮」读成
/// 「和能量」,这类切分不是本意;其余输入里简拼是正常打法,代价为 0。
const ABBREV_COST: f64 = 8.0;
/// 手误代价的倍数。下面的基础代价只表达手误之间的相对常见程度。
const TYPO_SCALE: f64 = 4.0;
/// 相邻字母调换、相邻键替换、漏键、多键的基础代价。
const TYPO_SWAP: f64 = 0.40;
const TYPO_NEAR_KEY: f64 = 0.60;
const TYPO_MISSED: f64 = 0.85;
const TYPO_EXTRA: f64 = 0.85;
/// 窗口本身已是合法音节时的额外代价:打出来的字母能读通,多半不是打错。
const TYPO_VALID_WINDOW: f64 = 1.5;
/// 整串能按全拼读完时的额外代价:输入完全合法,纠错只作后备。
const TYPO_COMPLETE_INPUT: f64 = 1.5;
/// 最短手误窗口。两字母窗口最容易和混打缩写撞车。
const TYPO_MIN_WINDOW: usize = 2;
/// 最长手误窗口:最长音节 6 字母加一个多打的键。
const TYPO_MAX_WINDOW: usize = 7;
const MAX_SYLL_LEN: usize = 6; // zhuang / chuang

const KEY_ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

/// 音节图上的一个单元。
#[derive(Clone, Debug)]
pub struct Unit {
    /// 规范音节,或简拼字母。
    pub text: String,
    /// 用户实际打出的音节。模糊读法与 `text` 不同:用户在原文读音下学过的词也算数。
    pub literal: String,
    pub abbrev: bool,
    /// 在原始串中的字节区间(不含两侧隔音符)。
    pub span: (usize, usize),
    /// 这种读法的代价(nat)。
    pub cost: f64,
    /// 模糊改动处数。
    pub edits: usize,
    /// 命中的模糊组位掩码。
    pub groups: u8,
    /// 由手误纠正得到:单元文本与原文不同,学习按单元文本。
    pub typo: bool,
}

impl Unit {
    fn plain(text: String, abbrev: bool, span: (usize, usize)) -> Self {
        Self {
            literal: text.clone(),
            text,
            abbrev,
            span,
            cost: 0.0,
            edits: 0,
            groups: 0,
            typo: false,
        }
    }
}

/// 字面读法:全拼音节与简拼,不含模糊音和手误。
fn literal(raw: &str) -> Vec<Vec<Unit>> {
    let b = raw.as_bytes();
    let mut out = vec![Vec::new(); b.len() + 1];
    for pos in 0..b.len() {
        if b[pos] == b'\'' {
            continue;
        }
        // 最长音节优先,让建图枚举先走更长的词。
        for len in (1..=MAX_SYLL_LEN.min(b.len() - pos)).rev() {
            if b[pos..pos + len].contains(&b'\'') {
                continue;
            }
            if let Ok(s) = std::str::from_utf8(&b[pos..pos + len])
                && is_syllable(s)
            {
                // `lue`/`nue` 落到词库使用的 `lve`/`nve`;span 仍指原文,
                // 显示与回删不受影响。
                out[pos].push(Unit::plain(
                    canonical(s).to_string(),
                    false,
                    (pos, pos + len),
                ));
            }
        }
        if let Ok(s) = std::str::from_utf8(&b[pos..pos + 1])
            && let Some(c) = s.chars().next()
            && c.is_ascii_lowercase()
            && ABBREV_LETTERS.contains(c)
        {
            out[pos].push(Unit::plain(s.to_string(), true, (pos, pos + 1)));
        }
        for d in ABBREV_DIGRAPHS {
            if b[pos..].starts_with(d.as_bytes()) {
                out[pos].push(Unit::plain((*d).to_string(), true, (pos, pos + 2)));
            }
        }
    }
    out
}

/// 只用全拼音节能否读完整串。
fn reads_as_full(raw: &str, edges: &[Vec<Unit>]) -> bool {
    let mut reach = vec![false; raw.len() + 1];
    reach[raw.len()] = true;
    for i in (0..raw.len()).rev() {
        reach[i] = if raw.as_bytes()[i] == b'\'' {
            reach[i + 1]
        } else {
            edges[i].iter().any(|e| !e.abbrev && reach[e.span.1])
        };
    }
    reach[0]
}

/// 整串能否只用全拼音节读完。
pub fn is_complete(raw: &str) -> bool {
    reads_as_full(raw, &literal(raw))
}

/// 建音节图。`typos` 为假时不加手误单元(快捷触发键要秒出)。
pub fn lattice(raw: &str, fuzzy: &Fuzzy, typos: bool) -> Vec<Vec<Unit>> {
    let plain = literal(raw);
    let complete = reads_as_full(raw, &plain);
    let mut out = vec![Vec::new(); raw.len() + 1];
    for (pos, units) in plain.into_iter().enumerate() {
        for unit in units {
            spread(unit, fuzzy, complete, &mut out[pos]);
        }
    }
    if typos {
        add_typos(raw, complete, &mut out);
    }
    out
}

/// 字面单元展开成它的模糊读法,自身在首位。
fn spread(mut unit: Unit, fuzzy: &Fuzzy, complete: bool, out: &mut Vec<Unit>) {
    if !unit.abbrev {
        for (text, edits, groups) in expansions_scored(&unit.text, fuzzy) {
            out.push(Unit {
                text,
                cost: FUZZY_COST * edits as f64,
                edits,
                groups,
                ..unit.clone()
            });
        }
        return;
    }
    if complete {
        unit.cost = ABBREV_COST;
    }
    let first = unit.text.chars().next().unwrap_or('\0');
    let letters = initial_alternatives_scored(first, fuzzy);
    for (letter, group) in letters.into_iter().skip(1) {
        out.push(Unit {
            text: letter.to_string(),
            cost: unit.cost + FUZZY_COST,
            edits: 1,
            groups: group,
            ..unit.clone()
        });
    }
    out.push(unit);
}

/// 查手误表:每个窗口如果是某音节的一处手误,就加一条带代价的单元。
fn add_typos(raw: &str, complete: bool, out: &mut [Vec<Unit>]) {
    let b = raw.as_bytes();
    let table = misspellings();
    let extra = if complete { TYPO_COMPLETE_INPUT } else { 0.0 };
    for pos in 0..b.len() {
        for len in TYPO_MIN_WINDOW..=TYPO_MAX_WINDOW.min(b.len() - pos) {
            let window = &b[pos..pos + len];
            if !window.iter().all(u8::is_ascii_lowercase) {
                break;
            }
            let Ok(window) = std::str::from_utf8(window) else {
                break;
            };
            let Some(intended) = table.get(window) else {
                continue;
            };
            let valid = if is_syllable(window) {
                TYPO_VALID_WINDOW
            } else {
                0.0
            };
            for &(syllable, cost) in intended {
                out[pos].push(Unit {
                    cost: cost * TYPO_SCALE + valid + extra,
                    typo: true,
                    ..Unit::plain(syllable.to_string(), false, (pos, pos + len))
                });
            }
        }
    }
}

/// 手误表:打出来的字母串 → 想打的音节及基础代价。按「想打的 → 打出的」方向生成,
/// 与真实信道同向;全部音节只枚举一次,查询时每个窗口一次哈希。
fn misspellings() -> &'static HashMap<String, Vec<(&'static str, f64)>> {
    static TABLE: OnceLock<HashMap<String, Vec<(&'static str, f64)>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table: HashMap<String, Vec<(&'static str, f64)>> = HashMap::new();
        for &syllable in SYLLABLES {
            // 模糊音是用户的发音习惯,有自己的代价和开关,不算手误。
            let accents = expansions_scored(syllable, &Fuzzy::auto());
            for (typed, cost) in slips(syllable) {
                if typed.len() < 2 || accents.iter().any(|accent| accent.0 == typed) {
                    continue;
                }
                let intended = table.entry(typed).or_default();
                match intended.iter_mut().find(|(known, _)| *known == syllable) {
                    Some(found) => found.1 = found.1.min(cost),
                    None => intended.push((syllable, cost)),
                }
            }
        }
        table
    })
}

fn letters(bytes: Vec<u8>) -> String {
    bytes.into_iter().map(char::from).collect()
}

/// 一个音节的全部单处手误。
fn slips(syllable: &str) -> Vec<(String, f64)> {
    let b = syllable.as_bytes();
    let mut out = Vec::new();
    for i in 0..b.len() {
        if i + 1 < b.len() && b[i] != b[i + 1] {
            let mut typed = b.to_vec();
            typed.swap(i, i + 1);
            out.push((letters(typed), TYPO_SWAP));
        }
        for (key, cost) in confusable(char::from(b[i])) {
            let mut typed = b.to_vec();
            typed[i] = key as u8;
            out.push((letters(typed), cost));
        }
        let mut typed = b.to_vec();
        typed.remove(i);
        out.push((letters(typed), TYPO_MISSED));
    }
    // 多打的键落在前后字母的邻键上,或者同一个键连按两次。
    for i in 0..=b.len() {
        let around = [i.checked_sub(1).map(|at| b[at]), b.get(i).copied()];
        for letter in around.into_iter().flatten() {
            let letter = char::from(letter);
            for key in neighbors(letter).into_iter().chain([letter]) {
                let mut typed = b.to_vec();
                typed.insert(i, key as u8);
                out.push((letters(typed), TYPO_EXTRA));
            }
        }
    }
    out
}

/// 某键容易被打成哪些键。n/l、f/h、r/l 属于模糊音,不在这里重复。
fn confusable(c: char) -> Vec<(char, f64)> {
    let phonetic: &[(char, f64)] = match c {
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
    let mut out = phonetic.to_vec();
    for key in neighbors(c) {
        if !out.iter().any(|(known, _)| *known == key) {
            out.push((key, TYPO_NEAR_KEY));
        }
    }
    out
}

/// 物理相邻的键:三排错开半格,距离不超过约一个键宽。
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
            let dx = x(row, col) - x(other_row, other_col);
            let dy = row as f32 - other_row as f32;
            if key != c && (dx * dx + dy * dy).sqrt() <= 1.15 {
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

fn x(row: usize, col: usize) -> f32 {
    col as f32 + row as f32 * 0.5
}

/// 判断当前输入是否为声母/简拼缩写序列。
///
/// 允许包含隔音符 `'` 与合法的简拼声母字母(含 zh/ch/sh)。
/// 当用户输入连续声母简拼(如 `wsm`、`hngydm`、`bswszh`)时返回 true。
pub fn is_abbrev_sequence(raw: &str) -> bool {
    if raw.is_empty() {
        return false;
    }
    let b = raw.as_bytes();
    let mut i = 0;
    let mut count = 0;
    while i < b.len() {
        if b[i] == b'\'' {
            i += 1;
            continue;
        }
        if i + 1 < b.len() && (b[i] == b'z' || b[i] == b'c' || b[i] == b's') && b[i + 1] == b'h' {
            i += 2;
            count += 1;
            continue;
        }
        let ch = b[i] as char;
        if ch.is_ascii_lowercase() && ABBREV_LETTERS.contains(ch) {
            i += 1;
            count += 1;
        } else {
            return false;
        }
    }
    count >= 2
}

/// 无候选时的回删粒度:按最长音节贪心走一遍图。
pub fn first_path(raw: &str) -> Vec<(usize, usize)> {
    let edges = literal(raw);
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < raw.len() {
        if raw.as_bytes()[pos] == b'\'' {
            pos += 1;
            continue;
        }
        match edges[pos].first() {
            Some(e) => {
                out.push(e.span);
                pos = e.span.1;
            }
            None => pos += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reads(raw: &str, pos: usize, text: &str) -> Option<Unit> {
        lattice(raw, &Fuzzy::auto(), true)[pos]
            .iter()
            .find(|unit| unit.text == text)
            .cloned()
    }

    #[test]
    fn swapped_letters_read_as_the_intended_syllable() {
        let edges = lattice("hoa", &Fuzzy::auto(), true);
        let unit = edges[0]
            .iter()
            .find(|unit| unit.text == "hao" && unit.span == (0, 3))
            .expect("hoa 读作 hao");
        assert!(unit.typo);
        assert!(unit.cost > 0.0);
    }

    #[test]
    fn exact_syllables_cost_nothing() {
        let unit = reads("nihao", 2, "hao").expect("hao 是合法音节");
        assert!(!unit.typo);
        assert_eq!(unit.cost, 0.0);
    }

    #[test]
    fn fuzzy_and_typo_share_one_scale() {
        let fuzzy = reads("zi", 0, "zhi").expect("zi 模糊到 zhi");
        assert_eq!(fuzzy.edits, 1);
        assert!(!fuzzy.typo);
        assert!((fuzzy.cost - FUZZY_COST).abs() < 1e-9);
    }

    #[test]
    fn abbreviations_are_free_unless_the_input_is_complete() {
        let mixed = literal("nh");
        assert!(!reads_as_full("nh", &mixed));
        assert_eq!(reads("nh", 1, "h").map(|unit| unit.cost), Some(0.0));
        assert_eq!(reads("hen", 2, "n").map(|unit| unit.cost), Some(ABBREV_COST));
    }

    #[test]
    fn apostrophes_stop_typo_windows() {
        let edges = lattice("ho'a", &Fuzzy::auto(), true);
        assert!(edges[0].iter().all(|unit| unit.span.1 <= 2));
    }
}
