//! 音节图:把原始输入串展开成「位置 → 单元」的边。
//!
//! 单元是全拼音节,或简拼(单字母、zh/ch/sh)。同一串输入的所有切分都是这张图
//! 上的路径,不再枚举成一条条完整切分,避免组合爆炸与路径上限漏解。
//! 隔音符只作硬边界,不产音节。

use lueying_dict::syllables::{ABBREV_DIGRAPHS, ABBREV_LETTERS, canonical, is_syllable};

use crate::typo;

/// 音节图上的一个单元。`abbrev` 为真表示简拼。
#[derive(Clone, Debug)]
pub struct Syll {
    pub text: String,
    pub abbrev: bool,
    /// 在原始串中的字节区间(不含两侧隔音符)。
    pub span: (usize, usize),
    /// 手误纠正罚分:单元是把原文一处改动(交换、替换、多打、漏打)得到的合法音节时为正,否则为 0。
    pub typo: f64,
}

const MAX_SYLL_LEN: usize = 6; // zhuang / chuang

/// 建音节图:`edges[i]` 是从字节位置 `i` 出发的全部单元。
pub fn edges(raw: &str) -> Vec<Vec<Syll>> {
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
                out[pos].push(Syll {
                    // `lue`/`nue` 落到词库使用的 `lve`/`nve`;span 仍指原文,
                    // 显示与回删不受影响。
                    text: canonical(s).to_string(),
                    abbrev: false,
                    span: (pos, pos + len),
                    typo: 0.0,
                });
            }
        }
        if let Ok(s) = std::str::from_utf8(&b[pos..pos + 1])
            && let Some(c) = s.chars().next()
            && c.is_ascii_lowercase()
            && ABBREV_LETTERS.contains(c)
        {
            out[pos].push(Syll {
                text: s.to_string(),
                abbrev: true,
                span: (pos, pos + 1),
                typo: 0.0,
            });
        }
        for d in ABBREV_DIGRAPHS {
            if b[pos..].starts_with(d.as_bytes()) {
                out[pos].push(Syll {
                    text: (*d).to_string(),
                    abbrev: true,
                    span: (pos, pos + 2),
                    typo: 0.0,
                });
            }
        }
    }
    out
}

/// 手误纠正单元:对每个 2 到 6 字母的窗口做一处改动(相邻字母交换、替换、多打、漏打),
/// 结果仍是合法音节的,作为带罚分的单元加进音节图。与 `typo::corrections` 的罚分同源。
/// 只在整串不能按全拼切完时调用,完整输入的音节图保持不变。
pub fn add_typo_edges(raw: &str, out: &mut [Vec<Syll>], min_window: usize) {
    let b = raw.as_bytes();
    for pos in 0..b.len() {
        if b[pos] == b'\'' {
            continue;
        }
        for len in min_window.max(2)..=MAX_SYLL_LEN.min(b.len() - pos) {
            let Ok(window) = std::str::from_utf8(&b[pos..pos + len]) else {
                continue;
            };
            if !window.bytes().all(|c| c.is_ascii_lowercase()) {
                continue;
            }
            for (text, cost) in typo::variants(window) {
                if is_syllable(&text) {
                    out[pos].push(Syll {
                        text: canonical(&text).to_string(),
                        abbrev: false,
                        span: (pos, pos + len),
                        typo: cost,
                    });
                }
            }
        }
    }
}

/// 整串能否只用全拼音节切完(不借助简拼)。用于判断用户是否完整打出了拼音:
/// 完整拼音串里不该让「半个音节当简拼」的读法抢占正常分词。
pub fn complete_full(raw: &str) -> bool {
    let edges = edges(raw);
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

/// 整串能否被音节图切完。错音纠正在建词典图前先用它过滤明显不合法的串。
pub fn complete(raw: &str) -> bool {
    let edges = edges(raw);
    let mut reach = vec![false; raw.len() + 1];
    reach[raw.len()] = true;
    for i in (0..raw.len()).rev() {
        reach[i] = if raw.as_bytes()[i] == b'\'' {
            reach[i + 1]
        } else {
            edges[i].iter().any(|e| reach[e.span.1])
        };
    }
    reach[0]
}

/// 判断当前输入是否为声母/简拼缩写序列。
///
/// 允许包含隔音符 `'` 与合法的简拼声母字母(含 zh/ch/sh)。
/// 当用户输入连续声母简拼(如 `wsm`、`hngydm`、`bswszh`)时返回 true,
/// 指引解码网格与重排流水线按缩写整句通道处理,不落入死板的单字截断或手误纠错。
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
    let edges = edges(raw);
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
