//! 形码方案:五笔、仓颉这类按字形取码的输入法。
//!
//! 码表是一张「码 → 字词」的表,表文件是纯文本,每行一条:
//!
//! ```text
//! # 码<TAB>字词<TAB>权重
//! nk<TAB>你<TAB>1000
//! ```
//!
//! 同一个码下可以有多条,权重大的排前面。码没敲完时,所有以它开头的码都
//! 进候选:敲全的那个码排在最前,其余按权重排。

use std::path::Path;

use lueying_ext::{CodeCandidate, Expansion, Scheme, SchemeManifest};

/// 码表里的一条:一个码对应一个字或词。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeEntry {
    /// 取码:小写字母。
    pub code: String,
    /// 上屏文本:一个字或一个词。
    pub text: String,
    /// 权重:同一个码下越大越靠前。
    pub weight: u64,
}

/// 码表:按前缀查码。
#[derive(Clone, Debug, Default)]
pub struct CodeTable {
    /// 按码排序的条目;同码相邻,前缀命中的区间因此连续。
    entries: Vec<CodeEntry>,
}

impl CodeTable {
    /// 表里的条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 表是不是空的。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 读表文件。每行 `码<TAB>字词<TAB>权重`;`#` 开头是注释,空行忽略。
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(Self::from_text(&text))
    }

    /// 从表文本建表。坏行跳过,不因为一行脏数据丢掉整张表。
    pub fn from_text(text: &str) -> Self {
        let mut entries: Vec<CodeEntry> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let (Some(code), Some(word)) = (parts.next(), parts.next()) else {
                continue;
            };
            let code = code.trim().to_lowercase();
            let word = word.trim();
            if code.is_empty() || word.is_empty() {
                continue;
            }
            let weight = parts
                .next()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(1);
            entries.push(CodeEntry {
                code,
                text: word.to_string(),
                weight,
            });
        }
        entries.sort_by(|a, b| a.code.cmp(&b.code).then_with(|| b.weight.cmp(&a.weight)));
        Self { entries }
    }

    /// 前缀查表:敲全的码排在最前,其余按权重排。
    ///
    /// 前缀没敲完时码表里的整码仍然进候选,`full` 标出哪些是敲全的。
    pub fn lookup(&self, prefix: &str) -> Vec<CodeCandidate> {
        if prefix.is_empty() {
            return Vec::new();
        }
        let start = self
            .entries
            .partition_point(|entry| entry.code.as_str() < prefix);
        let mut hits: Vec<&CodeEntry> = self.entries[start..]
            .iter()
            .take_while(|entry| entry.code.starts_with(prefix))
            .collect();
        hits.sort_by(|a, b| {
            let a_exact = a.code == prefix;
            let b_exact = b.code == prefix;
            b_exact.cmp(&a_exact).then_with(|| b.weight.cmp(&a.weight))
        });
        hits.into_iter()
            .map(|entry| CodeCandidate {
                text: entry.text.clone(),
                code: entry.code.clone(),
                weight: entry.weight,
                full: entry.code == prefix,
            })
            .collect()
    }
}

/// 形码方案:击键直接查码表,不走音节展开。
pub struct CodeScheme {
    id: &'static str,
    name: &'static str,
    table: std::sync::Arc<CodeTable>,
}

impl CodeScheme {
    /// 新建一个形码方案。`id` 与 `name` 进设置页与候选窗。
    pub fn new(id: &'static str, name: &'static str, table: CodeTable) -> Self {
        Self {
            id,
            name,
            table: std::sync::Arc::new(table),
        }
    }

    /// 码表。
    pub fn table(&self) -> &CodeTable {
        &self.table
    }
}

impl Scheme for CodeScheme {
    fn manifest(&self) -> SchemeManifest {
        SchemeManifest {
            id: self.id,
            name: self.name,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// 码输入不分音节;这一层只在方案被当成全拼用时才会走到。
    fn expand(&self, raw: &str) -> Expansion {
        Expansion::identity(raw)
    }

    /// 整串算一段:退格与 Ctrl+左右按整段走。
    fn segments(&self, raw: &str) -> Vec<(usize, usize)> {
        if raw.is_empty() {
            Vec::new()
        } else {
            vec![(0, raw.len())]
        }
    }

    fn code_candidates(&self, raw: &str) -> Vec<CodeCandidate> {
        self.table.lookup(raw)
    }
}

#[cfg(test)]
#[path = "code_tests.rs"]
mod tests;
