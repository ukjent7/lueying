//! 辅助码扩展:在拼音后按 `` ` `` 再打 1–2 位字形码,筛出字形相符的候选。
//!
//! 内置五笔单字码表(`data/wubi.txt`),也可以实现 [`AuxLookup`] 接入
//! 其他字形方案。辅助码只筛覆盖整个基础输入的候选,选中后连同辅码一起上屏;
//! 学习键仍是基础拼音,不会把辅码写进用户词库。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use lueying_ext::{CandidateFilter, Expansion, Scheme, SchemeManifest};

/// 字形码查询。返回该字是否有任一码以 `prefix` 开头。
pub trait AuxLookup: Send + Sync {
    fn matches(&self, text: &str, code: &str) -> bool;
}

/// 字 → 1–2 位字形码。内置表与自定义表共用这一结构。
pub struct AuxCode {
    codes: HashMap<char, Vec<String>>,
}

impl AuxCode {
    /// 空表:任何辅码都不命中。
    pub fn empty() -> Self {
        Self {
            codes: HashMap::new(),
        }
    }

    /// 从 `字=码1,码2` 行文本构造;坏行跳过。
    pub fn from_text(text: &str) -> Self {
        let mut codes: HashMap<char, Vec<String>> = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((text, code)) = line.split_once('=') else {
                continue;
            };
            let mut chars = text.trim().chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                continue;
            };
            let entries = codes.entry(ch).or_default();
            for item in code.split(',') {
                let item = item.trim().to_ascii_lowercase();
                if !item.is_empty()
                    && item.len() <= 2
                    && item.bytes().all(|b| b.is_ascii_alphabetic())
                    && !entries.iter().any(|existing| existing == &item)
                {
                    entries.push(item);
                }
            }
        }
        Self { codes }
    }

    /// 内置五笔表。
    pub fn builtin() -> Arc<Self> {
        static TABLE: OnceLock<Arc<AuxCode>> = OnceLock::new();
        Arc::clone(
            TABLE.get_or_init(|| Arc::new(AuxCode::from_text(include_str!("../data/wubi.txt")))),
        )
    }

    /// 收录的单字数。
    pub fn len(&self) -> usize {
        self.codes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    fn starts_with(&self, ch: char, prefix: &str) -> bool {
        self.codes
            .get(&ch)
            .is_some_and(|codes| codes.iter().any(|code| code.starts_with(prefix)))
    }
}

impl AuxLookup for AuxCode {
    fn matches(&self, text: &str, code: &str) -> bool {
        let mut chars = code.chars();
        let Some(first) = chars.next() else {
            return true;
        };
        let Some(second) = chars.next() else {
            return text
                .chars()
                .any(|ch| self.starts_with(ch, &first.to_string()));
        };
        let pair = format!("{first}{second}");
        if text.chars().any(|ch| self.starts_with(ch, &pair)) {
            return true;
        }
        // 两位辅码也接受「先一个字命中首位、后面的字命中次位」。
        let mut seen_first = false;
        for ch in text.chars() {
            if !seen_first {
                seen_first = self.starts_with(ch, &first.to_string());
            } else if self.starts_with(ch, &second.to_string()) {
                return true;
            }
        }
        false
    }
}

struct Filter {
    code: Arc<dyn AuxLookup>,
    aux: String,
}

impl CandidateFilter for Filter {
    fn matches(&self, text: &str) -> bool {
        self.code.matches(text, &self.aux)
    }
}

/// 给任意方案套上辅助码。基础方案负责拼音展开,本层只剥出 `` ` `` 之后的辅码。
pub struct AuxScheme {
    base: Arc<dyn Scheme>,
    code: Arc<dyn AuxLookup>,
}

impl AuxScheme {
    pub fn new(base: Arc<dyn Scheme>, code: Arc<dyn AuxLookup>) -> Self {
        Self { base, code }
    }

    fn split(raw: &str) -> Option<(&str, &str)> {
        raw.split_once('`')
    }
}

impl Scheme for AuxScheme {
    fn manifest(&self) -> SchemeManifest {
        let base = self.base.manifest();
        let (id, name) = match base.id {
            "xiaohe" => ("xiaohe-aux", "小鹤双拼 + 辅助码"),
            "ziranma" => ("ziranma-aux", "自然码 + 辅助码"),
            "mspy" => ("mspy-aux", "微软双拼 + 辅助码"),
            "sogou" => ("sogou-aux", "搜狗双拼 + 辅助码"),
            "full" => ("full-aux", "全拼 + 辅助码"),
            _ => ("custom-aux", "自定义方案 + 辅助码"),
        };
        SchemeManifest {
            id,
            name,
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn expand(&self, raw: &str) -> Expansion {
        let Some((base_raw, aux)) = Self::split(raw) else {
            return self.base.expand(raw);
        };
        let mut expansion = self.base.expand(base_raw);
        expansion.aux_start = Some(base_raw.len());
        let aux: String = aux
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .take(2)
            .flat_map(char::to_lowercase)
            .collect();
        if !aux.is_empty() {
            expansion.filter = Some(Arc::new(Filter {
                code: Arc::clone(&self.code),
                aux,
            }));
        }
        // 辅码段没有查询串对应;补齐契约要求的长度,统一指向基础输入末尾。
        expansion
            .raw_to_query
            .resize(raw.len() + 1, expansion.query.len());
        expansion.segments.push((base_raw.len(), raw.len()));
        expansion
    }

    fn segments(&self, raw: &str) -> Vec<(usize, usize)> {
        self.expand(raw).segments
    }

    fn accepts_char(&self, c: char) -> bool {
        c == '`' || self.base.accepts_char(c)
    }

    fn key_for_spans(&self, raw: &str, spans: &[(usize, usize)]) -> String {
        let Some((base_raw, _)) = Self::split(raw) else {
            return self.base.key_for_spans(raw, spans);
        };
        let base_len = base_raw.len();
        let spans: Vec<(usize, usize)> = spans
            .iter()
            .map(|&(start, end)| (start.min(base_len), end.min(base_len)))
            .filter(|(start, end)| start < end)
            .collect();
        self.base.key_for_spans(base_raw, &spans)
    }
}
