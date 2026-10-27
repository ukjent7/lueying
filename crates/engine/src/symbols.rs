//! v 模式符号表:输入 `vjt` 或 `v/jt` 取箭头等分类符号。
//!
//! 代码与符号来自 rime-frost 的 `symbols_v.yaml`,只保留常用分类;数字代码
//! 不与 `v123`/`v1+2` 争用,分类代码只认完整匹配,避免半截代码吞键。

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::query::Candidate;

/// 单个分类最多保留的符号数;超出部分对输入法候选窗没有实用价值。
const MAX_SYMBOLS: usize = 120;

pub(crate) fn candidates(raw: &str) -> Option<Vec<Candidate>> {
    let (_, name, symbols) = code(raw)?;
    let mut out = Vec::new();
    for (index, symbol) in symbols.split_whitespace().take(MAX_SYMBOLS).enumerate() {
        let mut cand = Candidate::quick(raw.len(), symbol, 1.0e9 - index as f64);
        cand.hint = Some(name.to_string());
        out.push(cand);
    }
    (!out.is_empty()).then_some(out)
}

pub(crate) fn is_trigger(raw: &str) -> bool {
    raw.starts_with('v') && code(raw).is_some()
}

fn code(raw: &str) -> Option<(&str, &'static str, &'static str)> {
    let body = raw.strip_prefix('v')?;
    let body = body.strip_prefix('/').unwrap_or(body);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    let (name, symbols) = *table().get(body)?;
    Some((body, name, symbols))
}

fn table() -> &'static HashMap<&'static str, (&'static str, &'static str)> {
    static TABLE: OnceLock<HashMap<&'static str, (&'static str, &'static str)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        include_str!("../data/symbols.tsv")
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\t');
                let code = parts.next()?;
                let name = parts.next()?;
                let symbols = parts.next()?;
                (!code.is_empty() && !name.is_empty() && !symbols.is_empty())
                    .then_some((code, (name, symbols)))
            })
            .collect()
    })
}
