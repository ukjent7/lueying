//! 拼音切分:把原始输入串(小写字母与隔音符)枚举成所有合法音节序列。
//!
//! 整串被完全切分才算一条路径;隔音符只作硬边界,不产音节。
//! 全拼与简拼(单字母或 zh/ch/sh)混合出现,同一串可能有多条路径(xian / xi'an / xia+n),
//! 由候选层跨切分合并排序。

use pinyin_dict::syllables::{ABBREV_DIGRAPHS, ABBREV_LETTERS, is_syllable};

/// 切分出的一个音节。`abbrev` 为真表示简拼。
#[derive(Clone, Debug)]
pub struct Syll {
    pub text: String,
    pub abbrev: bool,
    /// 在原始串中的字节区间(不含两侧隔音符)。
    pub span: (usize, usize),
}

const MAX_PATHS: usize = 64;
const MAX_SYLL_LEN: usize = 6; // zhuang / chuang

/// 枚举所有合法切分(最长音节优先的 DFS 顺序)。完全不可切分时为空。
pub fn segmentations(raw: &str) -> Vec<Vec<Syll>> {
    let mut out = Vec::new();
    dfs(raw.as_bytes(), 0, &mut Vec::new(), &mut out);
    out
}

fn dfs(b: &[u8], pos: usize, cur: &mut Vec<Syll>, out: &mut Vec<Vec<Syll>>) {
    if out.len() >= MAX_PATHS || pos > b.len() {
        return;
    }
    if pos == b.len() {
        out.push(cur.clone());
        return;
    }
    if b[pos] == b'\'' {
        if cur.is_empty() {
            return;
        }
        dfs(b, pos + 1, cur, out);
        return;
    }
    for l in (1..=MAX_SYLL_LEN.min(b.len() - pos)).rev() {
        if b[pos..pos + l].contains(&b'\'') {
            break;
        }
        if let Ok(s) = std::str::from_utf8(&b[pos..pos + l])
            && is_syllable(s)
        {
            cur.push(Syll {
                text: s.to_string(),
                abbrev: false,
                span: (pos, pos + l),
            });
            dfs(b, pos + l, cur, out);
            cur.pop();
        }
    }
    if let Ok(s) = std::str::from_utf8(&b[pos..pos + 1])
        && let Some(c) = s.chars().next()
        && c.is_ascii_lowercase()
        && ABBREV_LETTERS.contains(c)
    {
        cur.push(Syll {
            text: s.to_string(),
            abbrev: true,
            span: (pos, pos + 1),
        });
        dfs(b, pos + 1, cur, out);
        cur.pop();
    }
    for d in ABBREV_DIGRAPHS {
        if b[pos..].starts_with(d.as_bytes()) {
            cur.push(Syll {
                text: (*d).to_string(),
                abbrev: true,
                span: (pos, pos + 2),
            });
            dfs(b, pos + 2, cur, out);
            cur.pop();
        }
    }
}
