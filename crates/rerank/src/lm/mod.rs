//! 整句与按键重排模型的推理内核(纯 Rust,不依赖 ONNX Runtime)。
//!
//! # 来源
//!
//! 来自 <https://github.com/metasequoiaime/chinese-ime-lm>，提交
//! `1c90f5ee1b76ecf18d5171ef78a4dc076a0bb0d8`（2026-11-04，release `rerank-v2`），目录 `reference/`，
//! 对应 `reference` 0.2.1 的批式推理实现。许可 Apache-2.0，见 crate 根目录的 `LICENSE`。
//!
//! 复制运行所需的 `lib.rs`、`weights.rs` 与上游单元测试文件。本地保留
//! `prefix_tokens` 剥掉词表外字符（上游 #91）、`best_where_margin`（8M 同步重排
//! 要求明确领先），以及 `Variant`/`segment` 对按键模型的兼容改动。更新时整目录
//! 替换，并把这些改动重新接回。
//!
//! 另外两个文件是本项目新增的：`ort.rs` 读取上游按键模型发布的 ORT 平面缓冲，
//! `keystroke.rs` 把 `lm_step.ort` 的三个文件加载成同一套 [`SentenceModel`]。
//! 它们让 8M/25M 按键重排保持纯 Rust 推理，不引入 ONNX Runtime 原生依赖。
//!
//! `math.rs` 的 `dispatched!` 也是本地改动：内核按 CPU 运行时选 AVX2，结果与基线逐位相同。
//!
//! Reranks candidates the pinyin decoder assembled, using a character-level language model.
//!
//! The model is produced by [`neural/`](https://github.com/metasequoiaime/chinese-ime-lm/tree/main/neural), and the measurements that shaped this crate are recorded in [`docs/measurements.md`](https://github.com/metasequoiaime/chinese-ime-lm/blob/main/docs/measurements.md). Two of them are load-bearing and are enforced here rather than left to callers:
//!
//! Reranking applies only when the engine's leading candidate was assembled by the lattice or the
//! fallback decoder. An exact dictionary hit on the whole key carries corpus frequency the model
//! does not have, and overriding those costs 14 points of top-1 accuracy at every margin tried.
//!
//! Only candidates covering the whole key are comparable. The engine also returns prefixes, and a
//! summed log-probability is larger for fewer characters, so a mixed-length list would rank the
//! shortest candidate first every time regardless of quality. Scores are per character for the same
//! reason, and the caller restricts the set it passes in.

mod forward;
mod keystroke;
mod math;
mod model;
mod ort;
mod reranker;
mod weights;

pub use model::SentenceModel;
pub use reranker::{Reranker, should_rerank};

#[cfg(test)]
mod tests;

#[cfg(test)]
use forward::{HiddenState, Prefix, TailRun, TailStates, TerminalState};
#[cfg(test)]
use math::{
    LinearScratch, Matrix, erf, gelu, layer_norm, linear, linear_into, split_qkv,
    split_qkv_with_suffix, split_qkv_with_suffix_into,
};
#[cfg(test)]
use model::{BOS, Block, Config, FeedForwardScratch};
#[cfg(test)]
use reranker::{COMPARED, Compared, comparable};
#[cfg(test)]
use weights::Tensor;

#[cfg(test)]
thread_local! {
    // 只统计当前测试线程实际执行的查询位置和词表投影，不改变生产接口。
    static ATTENTION_POSITIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static VOCABULARY_PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
std::thread_local! {
    static FORWARD_POSITIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // 记录最近一次候选编码的缓冲地址，验证评分时直接转移所有权。
    static LAST_ENCODED_POINTER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LAST_ENCODED_LENGTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// What the caller knows about one candidate. Neither field can be worked out from the candidate
/// text, which is why they are asked for rather than derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateFacts {
    /// This candidate answers the whole key, rather than a prefix of it or a predictive completion
    /// running past it. Two candidates that both answer the key are alternatives to each other and
    /// are comparable **even when they are different numbers of characters**: `xian` reads as 现 or
    /// as 西安, and both consume the key. A prefix and a completion answer different keys, so
    /// promoting one over the other would not be a choice between readings.
    ///
    /// Character count cannot answer this. It happens to agree whenever a key has one segmentation,
    /// which is why it served as a proxy, and it disagrees exactly where correction matters.
    ///
    /// An engine that advances composition has already computed this, whether or not it calls it that: "does this candidate consume the whole key" is the same question as "after selecting it, is there more input left to answer". Do not write a second test for it — see [`docs/measurements.md`](https://github.com/metasequoiaime/chinese-ime-lm/blob/main/docs/measurements.md) for the mapping onto one engine's existing predicate, and for why deriving it from the candidate source does not work.
    pub answers_key: bool,
    /// This candidate is an exact dictionary hit **that the engine vouches for as evidence of what
    /// the user meant**, not merely a row that came out of a dictionary.
    ///
    /// Overriding such a hit loses accuracy — 0.719 against 0.690 over 2052 cases, at every margin
    /// tried — and the reason is that a dictionary answering the whole key carries corpus frequency
    /// the model does not have. That reason has a premise: the key it answered is the key the user
    /// meant to type.
    ///
    /// **An engine that corrects input breaks that premise and has to say so here.** When the user
    /// types `gongsi` meaning `gongshi`, 公司 is a perfectly exact hit on the characters that arrived,
    /// and its frequency is evidence for a word nobody asked for. The engine knows it expanded that
    /// key; the crate cannot. So this is not "did a dictionary match" — it is "should the model defer
    /// to this", and only the caller can answer it. An engine offering corrections of a key should
    /// report `false` for hits on the uncorrected reading of that same key.
    pub trusted_dictionary_hit: bool,
}

/// Why a model file was rejected.
///
/// Non-exhaustive because the loader is the first thing a hostile file meets, and a check added there later must not be a breaking change for every caller that matches on this.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelError {
    #[error("model file is truncated")]
    Truncated,
    #[error("model header: {0}")]
    Header(String),
    #[error("model is missing tensor {0}")]
    MissingTensor(String),
    #[error("model is missing metadata {0}")]
    MissingMetadata(String),
    #[error("model geometry: {0}")]
    Geometry(String),
}

/// `CandidateSource` values meaning an exact full-key hit in a dictionary. The ordering is defined by `quanpin/word_lattice.h` in the engine: `Database` is 0 and `UserDatabase` is 1.
///
/// This numbering belongs to that one engine and means nothing to any other. An engine with its own candidate sources should describe each candidate with [`CandidateFacts`] and call [`Reranker::best_where`], which never consults these values.
pub const DICTIONARY_SOURCES: [u8; 2] = [0, 1];

#[cfg(test)]
mod feed_forward_scratch_tests;

#[cfg(test)]
mod prefix_final_row_tests;

#[cfg(test)]
mod terminal_prefix_tests;

#[cfg(test)]
mod embedding_tests;

#[cfg(test)]
mod batched_tests;
