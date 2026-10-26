//! Reranks candidates the pinyin decoder assembled, using a character-level language model.
//!
//! The model is produced by `neural/`, and the measurements that shaped this crate are recorded in
//! `docs/measurements.md`. Two of them are load-bearing and are enforced here rather than left to callers:
//!
//! Reranking applies only when the engine's leading candidate was assembled by the lattice or the
//! fallback decoder. An exact dictionary hit on the whole key carries corpus frequency the model
//! does not have, and overriding those costs 14 points of top-1 accuracy at every margin tried.
//!
//! Only candidates covering the whole key are comparable. The engine also returns prefixes, and a
//! summed log-probability is larger for fewer characters, so a mixed-length list would rank the
//! shortest candidate first every time regardless of quality. Scores are per character for the same
//! reason, and the caller restricts the set it passes in.
//!
//! 文件布局:`sentence_model` 放前向与评分,`rerank` 放重排状态机,`tests` 放单元测试。

mod rerank;
mod sentence_model;
mod weights;

#[cfg(test)]
thread_local! {
    // 只统计当前测试线程实际执行的查询位置和词表投影，不改变生产接口。
    static ATTENTION_POSITIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static VOCABULARY_PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use weights::Tensor;

pub use rerank::{Reranker, should_rerank};

/// A weight matrix, kept in whatever precision the file stored it in.
///
/// Quantized matrices are not expanded on load. A per-output-row scale is a constant factor for the
/// whole row, so it lifts out of the dot product and multiplies the result once — identical
/// arithmetic, a quarter of the memory. The model runs inside an iOS keyboard extension, where
/// turning a 4.5 MB file into 24.5 MB resident is most of what the process is allowed.
struct Matrix {
    inner: Tensor,
}

impl Matrix {
    fn len(&self) -> usize {
        self.inner.len()
    }

    /// `dot(x, row) * scale[row]`, where the scale is 1 for an unquantized matrix.
    fn dot_row(&self, x: &[f32], row: usize, width: usize) -> f32 {
        let span = row * width..(row + 1) * width;
        match &self.inner {
            Tensor::Float(values) => dot(x, &values[span]),
            Tensor::Quantized { values, scales } => dot_i8(x, &values[span]) * scales[row],
        }
    }

    /// 将 token 与位置嵌入直接写入目标行；量化值先乘行缩放，再加位置。
    fn embed_row(&self, index: usize, position: &[f32], out: &mut [f32]) {
        let width = out.len();
        let span = index * width..(index + 1) * width;
        match &self.inner {
            Tensor::Float(values) => {
                for ((out, &token), &position) in out.iter_mut().zip(&values[span]).zip(position) {
                    *out = token + position;
                }
            }
            Tensor::Quantized { values, scales } => {
                let scale = scales[index];
                for ((out, &token), &position) in out.iter_mut().zip(&values[span]).zip(position) {
                    *out = f32::from(token) * scale + position;
                }
            }
        }
    }
}

/// `CandidateSource` values meaning an exact full-key hit in a dictionary. The ordering is defined
/// by `quanpin/word_lattice.h` in the engine: `Database` is 0 and `UserDatabase` is 1.
pub const DICTIONARY_SOURCES: [u8; 2] = [0, 1];

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
    /// An engine that advances composition has already computed this, whether or not it calls it
    /// that: "does this candidate consume the whole key" is the same question as "after selecting it,
    /// is there more input left to answer". Do not write a second test for it — see
    /// `docs/measurements.md` for the mapping onto one engine's existing predicate, and for why
    /// deriving it from the candidate source does not work.
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

/// How many comparable candidates are scored. The engine rarely returns more than a handful that
/// answer the same key — two in almost every real case — so this is a ceiling on the worst case
/// rather than a limit that normally binds.
const COMPARED: usize = 9;

/// The bounded set of candidate indices that answer the current key. Keeping the ceiling in the
/// type avoids allocating a temporary `Vec` before every reranking call.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Compared {
    indices: [usize; COMPARED],
    len: usize,
}

impl Compared {
    fn new() -> Self {
        Self {
            indices: [0; COMPARED],
            len: 0,
        }
    }

    fn push(&mut self, index: usize) {
        self.indices[self.len] = index;
        self.len += 1;
    }

    fn as_slice(&self) -> &[usize] {
        &self.indices[..self.len]
    }
}

const UNK: u32 = 1;
const BOS: u32 = 2;

#[cfg(test)]
std::thread_local! {
    static FORWARD_POSITIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // 记录最近一次候选编码的缓冲地址，验证评分时直接转移所有权。
    static LAST_ENCODED_POINTER: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LAST_ENCODED_LENGTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, thiserror::Error)]
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

#[derive(Debug, Clone, Copy, serde::Deserialize)]
struct Config {
    vocab: usize,
    n_layer: usize,
    n_head: usize,
    n_embd: usize,
    context: usize,
}

struct Block {
    ln1_weight: Vec<f32>,
    ln1_bias: Vec<f32>,
    qkv_weight: Matrix,
    qkv_bias: Vec<f32>,
    proj_weight: Matrix,
    proj_bias: Vec<f32>,
    ln2_weight: Vec<f32>,
    ln2_bias: Vec<f32>,
    fc_weight: Matrix,
    fc_bias: Vec<f32>,
    out_weight: Matrix,
    out_bias: Vec<f32>,
}

#[derive(Default)]
struct FeedForwardScratch {
    normalized: Vec<f32>,
    hidden: Vec<f32>,
}

pub struct SentenceModel {
    config: Config,
    index: HashMap<char, u32>,
    token: Matrix,
    position: Vec<f32>,
    blocks: Vec<Block>,
    final_weight: Vec<f32>,
    final_bias: Vec<f32>,
    attribution: String,
    license: String,
}

impl std::fmt::Debug for SentenceModel {
    /// The shape and provenance, never the weights: they run to tens of millions of values and
    /// nothing useful is learned from seeing them.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SentenceModel")
            .field("vocab", &self.config.vocab)
            .field("n_layer", &self.config.n_layer)
            .field("n_head", &self.config.n_head)
            .field("n_embd", &self.config.n_embd)
            .field("context", &self.config.context)
            .field("attribution", &self.attribution)
            .finish()
    }
}

impl std::fmt::Debug for Reranker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Reranker")
            .field("model", &self.model)
            .field("prefix_cached", &self.cached.is_some())
            .finish()
    }
}

// 状态只属于生成它的模型；分叉续跑共享归一化常数，不缓存整张词表。
struct HiddenState {
    values: Vec<f32>,
    log_normalizer: OnceLock<f32>,
}

impl HiddenState {
    fn new(values: Vec<f32>) -> Self {
        Self {
            values,
            log_normalizer: OnceLock::new(),
        }
    }
}

/// 上文持有各层 KV；普通候选续跑借用已有 KV，可见位置由 length 限定。
struct Prefix<'a> {
    length: usize,
    keys: Cow<'a, [Vec<f32>]>,
    values: Cow<'a, [Vec<f32>]>,
    /// 末位置续跑借用单行，直接在最终 KV 中拼接。
    terminal: Option<&'a TerminalState>,
    /// 前缀末位置预测候选首字，隐藏状态与它的归一化常数一起共享。
    last_hidden: Arc<HiddenState>,
}

// 一段已完成前向的位置；累计分仍由调用者按原顺序计算。
struct TailStates {
    keys: Vec<Vec<f32>>,
    values: Vec<Vec<f32>>,
    hidden: Vec<Arc<HiddenState>>,
}

// 末位置只保存新增的一行 KV，不重复持有整段前缀。
struct TerminalState {
    keys: Vec<Vec<f32>>,
    values: Vec<Vec<f32>>,
    hidden: Arc<HiddenState>,
}

/// Everything one candidate's run passed through, so that a later run agreeing with part of it can
/// start from where they stop agreeing instead of from the beginning.
struct TailRun {
    /// 上文占用的绝对位置数；KV 仅包含上文及已前向的位置，比累计分少一个候选位置。
    base: usize,
    keys: Vec<Vec<f32>>,
    values: Vec<Vec<f32>>,
    /// 已计算位置预测下一字的隐藏状态；末 token 只在续跑经过它时前向。
    hidden: Vec<Arc<HiddenState>>,
    /// 前 i+1 个 token 的完整累计对数概率，保留原来的累加分组。
    cumulative: Vec<f32>,
    pending: u32,
    terminal: OnceLock<TerminalState>,
}

impl TailRun {
    /// 恢复前 taken 个 token 后的前缀；已有 KV 和按需补算的末行都只借用。
    fn resume(&self, taken: usize, model: &SentenceModel) -> (Prefix<'_>, f32) {
        let n_embd = model.config.n_embd;
        debug_assert!(taken > 0 && taken <= self.cumulative.len());
        if taken > self.hidden.len() {
            let terminal = self.terminal.get_or_init(|| {
                let mut states = model.run_tail_states(
                    self.base + self.hidden.len(),
                    &self.keys,
                    &self.values,
                    None,
                    &[self.pending],
                );
                let width = (self.base + self.hidden.len()) * n_embd;
                TerminalState {
                    keys: states
                        .keys
                        .iter_mut()
                        .map(|row| row.split_off(width))
                        .collect(),
                    values: states
                        .values
                        .iter_mut()
                        .map(|row| row.split_off(width))
                        .collect(),
                    hidden: states.hidden.pop().expect("末位置已有前向结果"),
                }
            });
            return (
                Prefix {
                    length: self.base + taken,
                    keys: Cow::Borrowed(&self.keys),
                    values: Cow::Borrowed(&self.values),
                    terminal: Some(terminal),
                    last_hidden: terminal.hidden.clone(),
                },
                self.cumulative[taken - 1],
            );
        }
        (
            Prefix {
                length: self.base + taken,
                keys: Cow::Borrowed(&self.keys),
                values: Cow::Borrowed(&self.values),
                terminal: None,
                last_hidden: self.hidden[taken - 1].clone(),
            },
            self.cumulative[taken - 1],
        )
    }

    /// 把已有前 taken 个位置与 rest 拼为完整候选，供下次按键从任意共享位置续跑。
    fn joined(&self, taken: usize, carried: f32, rest: TailRun) -> TailRun {
        // resume 已补齐经过的末位置，joined 共享它，保留同一份归一化缓存。
        let terminal = if taken > self.hidden.len() {
            Some(
                self.terminal
                    .get()
                    .expect("拼接前已恢复末位置")
                    .hidden
                    .clone(),
            )
        } else {
            None
        };
        TailRun {
            base: self.base,
            keys: rest.keys,
            values: rest.values,
            hidden: self
                .hidden
                .iter()
                .take(taken)
                .cloned()
                .chain(terminal)
                .chain(rest.hidden)
                .collect(),
            cumulative: self.cumulative[..taken]
                .iter()
                .copied()
                .chain(rest.cumulative.iter().map(|value| carried + value))
                .collect(),
            pending: rest.pending,
            terminal: rest.terminal,
        }
    }
}

/// Which candidates may be compared, or `None` when the decision should be declined.
///
/// Separate from scoring and free of the model, because this is the part that decides what the
/// measurements mean: every published accuracy number for this crate is a consequence of which
/// candidates were allowed into the comparison, so it is worth being able to test on its own.
fn comparable(count: usize, facts: &impl Fn(usize) -> CandidateFacts) -> Option<Compared> {
    if count == 0 {
        return None;
    }
    let leader = facts(0);
    // An exact hit on the typed key carries frequency evidence the model does not have. A leader
    // that does not answer the key leaves nothing to promote it over: the comparison would be
    // between readings of different keys.
    if leader.trusted_dictionary_hit || !leader.answers_key {
        return None;
    }
    let mut considered = Compared::new();
    for index in (0..count).filter(|&index| facts(index).answers_key) {
        if considered.len == COMPARED {
            break;
        }
        considered.push(index);
    }
    (considered.len >= 2).then_some(considered)
}

/// Independent accumulators rather than one running sum.
///
/// A single accumulator makes every multiply-add wait for the previous one, so the loop runs at the
/// latency of the fused multiply-add instead of its throughput and leaves most of the core idle.
/// Eight partial sums give the pipeline eight independent chains to interleave. The summation order
/// differs from a serial reduction, which is immaterial here: the result feeds a ranking, and the
/// order was never guaranteed to match the trainer's anyway.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    const LANES: usize = 8;
    let mut partial = [0.0f32; LANES];
    let left = a.chunks_exact(LANES);
    let right = b.chunks_exact(LANES);
    let (tail_left, tail_right) = (left.remainder(), right.remainder());
    for (x, y) in left.zip(right) {
        for lane in 0..LANES {
            partial[lane] += x[lane] * y[lane];
        }
    }
    let mut total = partial.iter().sum::<f32>();
    for (x, y) in tail_left.iter().zip(tail_right) {
        total += x * y;
    }
    total
}

/// `out[m][o] = dot(x[m], w[o]) + bias[o]`, with both operands laid out row-major so every dot
/// product runs over contiguous memory.
#[cfg(test)]
fn linear(x: &[f32], w: &Matrix, bias: &[f32], inputs: usize, outputs: usize) -> Vec<f32> {
    let mut out = vec![0.0; x.len() / inputs * outputs];
    linear_into(x, w, bias, inputs, outputs, &mut out);
    out
}

fn linear_into(
    x: &[f32],
    w: &Matrix,
    bias: &[f32],
    inputs: usize,
    outputs: usize,
    out: &mut [f32],
) {
    let rows = x.len() / inputs;
    for row in 0..rows {
        let source = &x[row * inputs..(row + 1) * inputs];
        let target = &mut out[row * outputs..(row + 1) * outputs];
        for (index, slot) in target.iter_mut().enumerate() {
            *slot = w.dot_row(source, index, inputs) + bias[index];
        }
    }
}

/// The quantized counterpart of `dot`, with the same eight independent accumulators and the same
/// reason for them. The row's scale is applied by the caller, once, rather than to every value:
/// that is the whole point of keeping the weights as `i8`.
fn dot_i8(a: &[f32], b: &[i8]) -> f32 {
    const LANES: usize = 8;
    let mut partial = [0.0f32; LANES];
    let left = a.chunks_exact(LANES);
    let right = b.chunks_exact(LANES);
    let (tail_left, tail_right) = (left.remainder(), right.remainder());
    for (x, y) in left.zip(right) {
        for lane in 0..LANES {
            partial[lane] += x[lane] * f32::from(y[lane]);
        }
    }
    let mut total = partial.iter().sum::<f32>();
    for (x, y) in tail_left.iter().zip(tail_right) {
        total += x * f32::from(*y);
    }
    total
}

#[cfg(test)]
fn layer_norm(x: &[f32], weight: &[f32], bias: &[f32], width: usize) -> Vec<f32> {
    let mut out = vec![0.0; x.len()];
    layer_norm_into(x, weight, bias, width, &mut out);
    out
}

fn layer_norm_into(x: &[f32], weight: &[f32], bias: &[f32], width: usize, out: &mut [f32]) {
    for (source, target) in x.chunks_exact(width).zip(out.chunks_exact_mut(width)) {
        let mean = source.iter().sum::<f32>() / width as f32;
        let variance = source
            .iter()
            .map(|value| (value - mean) * (value - mean))
            .sum::<f32>()
            / width as f32;
        let inverse = 1.0 / (variance + 1e-5).sqrt();
        for (index, slot) in target.iter_mut().enumerate() {
            *slot = (source[index] - mean) * inverse * weight[index] + bias[index];
        }
    }
}

/// 按最终大小拆分，新增 K、V 直接写入已有可见前缀之后。
#[cfg(test)]
fn split_qkv(
    qkv: &[f32],
    rows: usize,
    n_embd: usize,
    prefix_keys: &[f32],
    prefix_values: &[f32],
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    split_qkv_with_suffix(qkv, rows, n_embd, prefix_keys, prefix_values, None)
}

/// 可选末行与旧前缀一起写入最终 K、V，不生成连续的临时前缀。
#[cfg(test)]
fn split_qkv_with_suffix(
    qkv: &[f32],
    rows: usize,
    n_embd: usize,
    prefix_keys: &[f32],
    prefix_values: &[f32],
    suffix: Option<(&[f32], &[f32])>,
) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let mut query = Vec::with_capacity(rows * n_embd);
    let (keys, values) = split_qkv_with_suffix_into(
        qkv,
        rows,
        n_embd,
        prefix_keys,
        prefix_values,
        suffix,
        &mut query,
    );
    (query, keys, values)
}

/// 把 query 写入调用者提供的 scratch，并按既有顺序返回最终 K、V。
fn split_qkv_with_suffix_into(
    qkv: &[f32],
    rows: usize,
    n_embd: usize,
    prefix_keys: &[f32],
    prefix_values: &[f32],
    suffix: Option<(&[f32], &[f32])>,
    query: &mut Vec<f32>,
) -> (Vec<f32>, Vec<f32>) {
    let (last_keys, last_values) = suffix.unwrap_or((&[], &[]));
    let width = rows * n_embd;
    query.clear();
    query.reserve(width);
    let mut k = Vec::with_capacity(prefix_keys.len() + last_keys.len() + width);
    let mut v = Vec::with_capacity(prefix_values.len() + last_values.len() + width);
    k.extend_from_slice(prefix_keys);
    v.extend_from_slice(prefix_values);
    k.extend_from_slice(last_keys);
    v.extend_from_slice(last_values);
    for row in 0..rows {
        let source = &qkv[row * 3 * n_embd..(row + 1) * 3 * n_embd];
        query.extend_from_slice(&source[..n_embd]);
        k.extend_from_slice(&source[n_embd..2 * n_embd]);
        v.extend_from_slice(&source[2 * n_embd..]);
    }
    (k, v)
}

/// The exact GELU, matching `torch.nn.functional.gelu` with its default settings. The tanh
/// approximation is a different function and the model was not trained with it.
fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + erf(x * std::f32::consts::FRAC_1_SQRT_2))
}

/// Abramowitz and Stegun 7.1.26, whose error stays below 1.5e-7 across the range.
fn erf(x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_6
            + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    sign * (1.0 - poly * (-x * x).exp())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod feed_forward_scratch_tests;

#[cfg(test)]
mod prefix_final_row_tests;

#[cfg(test)]
mod terminal_prefix_tests;

#[cfg(test)]
mod embedding_tests;

#[cfg(test)]
mod bounded_encoding_tests;

#[cfg(test)]
mod project_rows_tests;
