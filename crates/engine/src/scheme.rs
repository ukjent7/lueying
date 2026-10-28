//! 输入方案扩展点:把击键串展开成核心拼音引擎能查询的规范全拼。
//!
//! 默认全拼直接透传。双拼、直接辅助码等改变「击键 → 音节」映射的方案实现
//! [`Scheme`],由宿主在创建 [`crate::Engine`] 时注入;核心词图、语言模型与学习
//! 仍只认规范全拼,方案之间因此共享同一份用户词库。
//!
//! [`Expansion`] 同时给出两张位置映射:查询串每个字节边界对应的原始击键边界,
//! 以及原始击键边界对应的查询串边界。候选的 `spans` 先由查询串映射回原始
//! 击键,上屏时仍按用户实际敲的键切分;学习键则从规范全拼生成,换方案后仍能
//! 命中同一个词。

use std::sync::Arc;

use lueying_dict::syllables;

use crate::segment;

/// 方案声明。宿主用它展示当前方案并做配置兼容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemeManifest {
    pub id: &'static str,
    pub name: &'static str,
    pub version: &'static str,
}

/// 候选过滤。辅助码在方案层解析出辅码后,用它在核心候选里筛出字形相符的词。
pub trait CandidateFilter: Send + Sync {
    fn matches(&self, text: &str) -> bool;
}

/// 一次击键展开的结果。
#[derive(Clone)]
pub struct Expansion {
    /// 交给词图查询的规范全拼串,可含 `'` 与未打完的末音节。
    pub query: String,
    /// 查询串每个字节边界 → 原始击键字节边界;长度 `query.len() + 1`。
    pub raw_at: Vec<usize>,
    /// 原始击键每个字节边界 → 查询串字节边界;长度 `raw.len() + 1`。
    pub raw_to_query: Vec<usize>,
    /// 置顶与跨方案索引用的规范键。全拼保留原始键,双拼用展开后的音节串。
    pub key: String,
    /// 原始击键上的音节区间,供 Ctrl+左右与 Ctrl+Backspace 使用。
    pub segments: Vec<(usize, usize)>,
    /// 可选的候选过滤器(辅助码)。
    pub filter: Option<Arc<dyn CandidateFilter>>,
    /// 辅助码分隔符在原始击键中的位置。存在时,覆盖整个基础输入的候选
    /// 会把分隔符与辅码一起吃掉。
    pub aux_start: Option<usize>,
}

impl Expansion {
    /// 查询串原样透传,学习键按音节规范化:全拼与不改变切分的方案使用。
    ///
    /// 学习键不能直接用原始串——同一句话打不打隔音符会得到两个键,换到双拼
    /// 方案后也认不出来。按音节图切片再归一化,所有方案落到同一份用户词库。
    pub fn identity(raw: &str) -> Self {
        let segments = segment::first_path(raw);
        let key = segments
            .iter()
            .map(|&(start, end)| syllables::canonical(&raw[start..end]))
            .collect::<Vec<_>>()
            .join("'");
        Self {
            query: raw.to_string(),
            raw_at: (0..=raw.len()).collect(),
            raw_to_query: (0..=raw.len()).collect(),
            key,
            segments,
            filter: None,
            aux_start: None,
        }
    }
}

/// 输入方案。实现者只负责击键到规范全拼的映射,不接触词典或候选排序。
pub trait Scheme: Send + Sync {
    fn manifest(&self) -> SchemeManifest;

    /// 展开一次组合输入。`raw` 为小写字母、隔音符与方案允许的符号键。
    fn expand(&self, raw: &str) -> Expansion;

    /// 原始击键上的音节区间。默认按全拼音节图切分。
    fn segments(&self, raw: &str) -> Vec<(usize, usize)> {
        segment::first_path(raw)
    }

    /// 组合中是否接受这个符号键(如微软双拼的 `;` 与辅助码的 `` ` ``)。
    fn accepts_char(&self, _c: char) -> bool {
        false
    }

    /// 把原始击键上的若干区间换成规范学习键。默认逐段做全拼归一化。
    fn key_for_spans(&self, raw: &str, spans: &[(usize, usize)]) -> String {
        spans
            .iter()
            .map(|&(start, end)| syllables::canonical(&raw[start..end]))
            .collect::<Vec<_>>()
            .join("'")
    }
}

/// 默认全拼方案。
pub struct FullPinyin;

impl Scheme for FullPinyin {
    fn manifest(&self) -> SchemeManifest {
        SchemeManifest {
            id: "full",
            name: "全拼",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    fn expand(&self, raw: &str) -> Expansion {
        Expansion::identity(raw)
    }
}
