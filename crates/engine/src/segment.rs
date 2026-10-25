//! 音节图:把原始输入串展开成「位置 → 单元」的边。
//!
//! 单元是全拼音节,或简拼(单字母、zh/ch/sh)。同一串输入的所有切分都是这张图
//! 上的路径,不再枚举成一条条完整切分,避免组合爆炸与路径上限漏解。
//! 隔音符只作硬边界,不产音节。

use pinyin_dict::syllables::{ABBREV_DIGRAPHS, ABBREV_LETTERS, is_syllable};

/// 音节图上的一个单元。`abbrev` 为真表示简拼。
#[derive(Clone, Debug)]
pub struct Syll {
    pub text: String,
    pub abbrev: bool,
    /// 在原始串中的字节区间(不含两侧隔音符)。
    pub span: (usize, usize),
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
                    text: s.to_string(),
                    abbrev: false,
                    span: (pos, pos + len),
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
            });
        }
        for d in ABBREV_DIGRAPHS {
            if b[pos..].starts_with(d.as_bytes()) {
                out[pos].push(Syll {
                    text: (*d).to_string(),
                    abbrev: true,
                    span: (pos, pos + 2),
                });
            }
        }
    }
    out
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
