//! 前向引擎与候选续跑状态。
//!
//! 逐层前向、批式评分,以及跨按键复用的前缀 KV、末行终态与续跑记录;
//! [`crate::reranker`] 只规划批次,真正的算术都在这里。

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use crate::math::{
    HALF_BLOCK, ROW_BLOCK, dot, dot_block, gelu, gelu_tanh, layer_norm_into, linear_into,
    log_normalizer, row_blocks, split_qkv_with_suffix_into,
};
use crate::model::{Block, FeedForwardScratch, SentenceModel};
use crate::weights::Tensor;
#[cfg(test)]
use crate::{ATTENTION_POSITIONS, FORWARD_POSITIONS, VOCABULARY_PROJECTIONS};
// 状态只属于生成它的模型；分叉续跑共享归一化常数，不缓存整张词表。
pub(crate) struct HiddenState {
    pub(crate) values: Vec<f32>,
    pub(crate) log_normalizer: OnceLock<f32>,
}

impl HiddenState {
    pub(crate) fn new(values: Vec<f32>) -> Self {
        Self {
            values,
            log_normalizer: OnceLock::new(),
        }
    }
}

/// 上文持有各层 KV；普通候选续跑借用已有 KV，可见位置由 length 限定。
pub(crate) struct Prefix<'a> {
    pub(crate) length: usize,
    pub(crate) keys: Cow<'a, [Vec<f32>]>,
    pub(crate) values: Cow<'a, [Vec<f32>]>,
    /// 末位置续跑借用单行，直接在最终 KV 中拼接。
    pub(crate) terminal: Option<&'a TerminalState>,
    /// 前缀末位置预测候选首字，隐藏状态与它的归一化常数一起共享。
    pub(crate) last_hidden: Arc<HiddenState>,
}

// 一段已完成前向的位置；累计分仍由调用者按原顺序计算。
#[cfg(test)]
pub(crate) struct TailStates {
    pub(crate) keys: Vec<Vec<f32>>,
    pub(crate) values: Vec<Vec<f32>>,
    pub(crate) hidden: Vec<Arc<HiddenState>>,
}

// 末位置只保存新增的一行 KV，不重复持有整段前缀。
pub(crate) struct TerminalState {
    pub(crate) keys: Vec<Vec<f32>>,
    pub(crate) values: Vec<Vec<f32>>,
    pub(crate) hidden: Arc<HiddenState>,
}

/// Everything one candidate's run passed through, so that a later run agreeing with part of it can
/// start from where they stop agreeing instead of from the beginning.
pub(crate) struct TailRun {
    /// 上文占用的绝对位置数；KV 仅包含上文及已前向的位置，比累计分少一个候选位置。
    pub(crate) base: usize,
    pub(crate) keys: Vec<Vec<f32>>,
    pub(crate) values: Vec<Vec<f32>>,
    /// 已计算位置预测下一字的隐藏状态；末 token 只在续跑经过它时前向。
    pub(crate) hidden: Vec<Arc<HiddenState>>,
    /// 前 i+1 个 token 的完整累计对数概率，保留原来的累加分组。
    pub(crate) cumulative: Vec<f32>,
    pub(crate) pending: u32,
    pub(crate) terminal: OnceLock<TerminalState>,
}

/// One candidate at a time: how runs were continued before a decision was planned and run as one
/// batch. Kept as the reference the batched path is tested against bit for bit.
#[cfg(test)]
impl TailRun {
    /// 恢复前 taken 个 token 后的前缀；已有 KV 和按需补算的末行都只借用。
    pub(crate) fn resume(&self, taken: usize, model: &SentenceModel) -> (Prefix<'_>, f32) {
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
    pub(crate) fn joined(&self, taken: usize, carried: f32, rest: TailRun) -> TailRun {
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

impl SentenceModel {
    /// Runs the prefix and keeps every layer's keys and values.
    pub(crate) fn run_prefix(&self, tokens: &[u32]) -> Prefix<'static> {
        let n_embd = self.config.n_embd;
        let mut x = self.embed(tokens, 0);
        let mut feed_forward = FeedForwardScratch::default();
        let mut keys = Vec::with_capacity(self.blocks.len());
        let mut values = Vec::with_capacity(self.blocks.len());
        for (layer, block) in self.blocks.iter().enumerate() {
            self.normalize_attention_into(&x, block, &mut feed_forward);
            self.qkv_into(block, &mut feed_forward);
            let (k, v) = split_qkv_with_suffix_into(
                &feed_forward.hidden,
                tokens.len(),
                n_embd,
                &[],
                &[],
                None,
                &mut feed_forward.normalized,
            );
            // 最后一层只消费末位置输出；所有位置的 KV 仍供后续候选使用。
            let first = if layer + 1 == self.blocks.len() {
                tokens.len() - 1
            } else {
                0
            };
            let start = first * n_embd;
            let attended = self.attention(
                &feed_forward.normalized[start..],
                &k,
                &v,
                tokens.len() - first,
                first,
            );
            self.project_attention_into(&attended, block, &mut feed_forward);
            for (slot, &value) in x[start..].iter_mut().zip(&feed_forward.normalized) {
                *slot += value;
            }
            self.feed_forward(&mut x[start..], block, &mut feed_forward);
            keys.push(k);
            values.push(v);
        }
        let last = (tokens.len() - 1) * n_embd;
        feed_forward.normalized.resize(n_embd, 0.0);
        layer_norm_into(
            &x[last..],
            &self.final_weight,
            &self.final_bias,
            n_embd,
            &mut feed_forward.normalized,
        );
        let last_hidden = Arc::new(HiddenState::new(feed_forward.normalized));
        Prefix {
            length: tokens.len(),
            keys: Cow::Owned(keys),
            values: Cow::Owned(values),
            terminal: None,
            last_hidden,
        }
    }

    /// Runs a candidate against a cached prefix and returns its total log-probability.
    #[cfg(test)]
    pub(crate) fn score_tail(&self, prefix: &Prefix<'_>, tail: &[u32]) -> f32 {
        let run = self.run_tail(prefix, tail);
        run.cumulative[run.cumulative.len() - 1]
    }

    /// 完整累计分只需要前 length-1 个隐藏状态；末位置留给真正经过它的续跑。
    ///
    /// 分段累计仍沿用既有分组：carried + rest，不改变 token 的先后顺序。
    #[cfg(test)]
    pub(crate) fn run_tail(&self, prefix: &Prefix<'_>, tail: &[u32]) -> TailRun {
        let states = self.run_tail_states(
            prefix.length,
            &prefix.keys,
            &prefix.values,
            prefix.terminal,
            &tail[..tail.len() - 1],
        );
        let mut running = self.log_probability(&prefix.last_hidden, tail[0]);
        let mut cumulative = Vec::with_capacity(tail.len());
        cumulative.push(running);
        for (state, &next) in states.hidden.iter().zip(&tail[1..]) {
            running += self.log_probability(state, next);
            cumulative.push(running);
        }
        TailRun {
            base: prefix.length,
            keys: states.keys,
            values: states.values,
            hidden: states.hidden,
            cumulative,
            pending: *tail.last().expect("候选至少有一个 token"),
            terminal: OnceLock::new(),
        }
    }

    /// 仅计算需要的因果位置；空段只保留上文 KV，单字评分无需前向。
    #[cfg(test)]
    pub(crate) fn run_tail_states(
        &self,
        length: usize,
        prefix_keys: &[Vec<f32>],
        prefix_values: &[Vec<f32>],
        terminal: Option<&TerminalState>,
        tokens: &[u32],
    ) -> TailStates {
        // 借用的旧 KV 可能更长，只复制本次续跑可见的位置。
        let n_embd = self.config.n_embd;
        let prefix_width = (length - usize::from(terminal.is_some())) * n_embd;
        if tokens.is_empty() {
            let copy = |rows: &[Vec<f32>], last: Option<&[Vec<f32>]>| {
                rows.iter()
                    .enumerate()
                    .map(|(layer, row)| {
                        let mut out = Vec::with_capacity(length * n_embd);
                        out.extend_from_slice(&row[..prefix_width]);
                        if let Some(last) = last {
                            out.extend_from_slice(&last[layer]);
                        }
                        out
                    })
                    .collect()
            };
            return TailStates {
                keys: copy(prefix_keys, terminal.map(|last| last.keys.as_slice())),
                values: copy(prefix_values, terminal.map(|last| last.values.as_slice())),
                hidden: Vec::new(),
            };
        }
        let mut x = self.embed(tokens, length);
        let mut feed_forward = FeedForwardScratch::default();
        let mut keys_out = Vec::with_capacity(self.blocks.len());
        let mut values_out = Vec::with_capacity(self.blocks.len());
        for (layer, block) in self.blocks.iter().enumerate() {
            self.normalize_attention_into(&x, block, &mut feed_forward);
            self.qkv_into(block, &mut feed_forward);
            let (keys, values) = split_qkv_with_suffix_into(
                &feed_forward.hidden,
                tokens.len(),
                n_embd,
                &prefix_keys[layer][..prefix_width],
                &prefix_values[layer][..prefix_width],
                terminal.map(|last| (last.keys[layer].as_slice(), last.values[layer].as_slice())),
                &mut feed_forward.normalized,
            );
            let attended = self.attention(
                &feed_forward.normalized,
                &keys,
                &values,
                tokens.len(),
                length,
            );
            self.project_attention_into(&attended, block, &mut feed_forward);
            for (slot, &value) in x.iter_mut().zip(&feed_forward.normalized) {
                *slot += value;
            }
            self.feed_forward(&mut x, block, &mut feed_forward);
            keys_out.push(keys);
            values_out.push(values);
        }
        feed_forward.normalized.resize(x.len(), 0.0);
        layer_norm_into(
            &x,
            &self.final_weight,
            &self.final_bias,
            n_embd,
            &mut feed_forward.normalized,
        );

        TailStates {
            keys: keys_out,
            values: values_out,
            hidden: feed_forward
                .normalized
                .chunks_exact(n_embd)
                .map(|row| Arc::new(HiddenState::new(row.to_vec())))
                .collect(),
        }
    }

    /// Runs a batch of positions that each continue `prefix` or an earlier row of the batch, and
    /// returns every row's final normalized hidden state, `rows × n_embd`.
    ///
    /// This is `run_tail_states` for many candidates at once. A row attends to the prefix and then
    /// to its own chain of parents, exactly the positions a single candidate's run would have
    /// before it, so each row's state is bit-for-bit what that run computes; what changes is that
    /// the matrix products see every row of the decision together and can use the blocked kernel.
    pub(crate) fn run_batch(
        &self,
        contexts: &[BatchContext<'_>],
        rows: &[BatchRow],
    ) -> BatchStates {
        let n_embd = self.config.n_embd;
        if rows.is_empty() {
            return BatchStates {
                keys: vec![Vec::new(); self.blocks.len()],
                values: vec![Vec::new(); self.blocks.len()],
                hidden: Vec::new(),
                chains: Vec::new(),
            };
        }
        #[cfg(test)]
        FORWARD_POSITIONS.with(|count| count.set(count.get() + rows.len()));
        // Every row's chain of batch rows, root first. Parents always come earlier in the batch.
        let mut chains: Vec<Vec<usize>> = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let mut chain = match row.parent {
                Some(parent) => {
                    debug_assert!(parent < index && rows[parent].depth + 1 == row.depth);
                    debug_assert_eq!(rows[parent].context, row.context);
                    chains[parent].clone()
                }
                None => {
                    debug_assert_eq!(row.depth, 0);
                    Vec::new()
                }
            };
            chain.push(index);
            chains.push(chain);
        }

        let mut x = vec![0.0; rows.len() * n_embd];
        for (row, slot) in rows.iter().zip(x.chunks_exact_mut(n_embd)) {
            let token = (row.token as usize).min(self.config.vocab - 1);
            let position =
                (contexts[row.context].length() + row.depth).min(self.config.context - 1);
            let position_row = &self.position[position * n_embd..(position + 1) * n_embd];
            self.token.embed_row(token, position_row, slot);
            if let Some(segment) = &self.segment {
                for (value, &added) in slot.iter_mut().zip(segment) {
                    *value += added;
                }
            }
        }

        let mut feed_forward = FeedForwardScratch::default();
        let mut keys = Vec::with_capacity(self.blocks.len());
        let mut values = Vec::with_capacity(self.blocks.len());
        let mut attended = vec![0.0; rows.len() * n_embd];
        let longest = rows
            .iter()
            .map(|row| contexts[row.context].length() + row.depth + 1)
            .max()
            .unwrap_or(0);
        let mut scores = vec![0.0; longest];
        for (layer, block) in self.blocks.iter().enumerate() {
            self.normalize_attention_into(&x, block, &mut feed_forward);
            self.qkv_into(block, &mut feed_forward);
            let mut layer_keys = Vec::with_capacity(rows.len() * n_embd);
            let mut layer_values = Vec::with_capacity(rows.len() * n_embd);
            for qkv in feed_forward.hidden.chunks_exact(3 * n_embd) {
                layer_keys.extend_from_slice(&qkv[n_embd..2 * n_embd]);
                layer_values.extend_from_slice(&qkv[2 * n_embd..]);
            }
            #[cfg(test)]
            ATTENTION_POSITIONS.with(|count| count.set(count.get() + rows.len()));
            attended.iter_mut().for_each(|slot| *slot = 0.0);
            for (((index, row), chain), target) in rows
                .iter()
                .enumerate()
                .zip(&chains)
                .zip(attended.chunks_exact_mut(n_embd))
            {
                let context = contexts[row.context];
                let length = context.length();
                let terminal = context
                    .terminal
                    .map(|last| (last.keys[layer].as_slice(), last.values[layer].as_slice()));
                let query = &feed_forward.hidden[index * 3 * n_embd..index * 3 * n_embd + n_embd];
                let sequence = Sequence {
                    stored: context.stored,
                    length,
                    chain,
                    width: n_embd,
                };
                self.attend(
                    query,
                    length + row.depth + 1,
                    |step| {
                        sequence.row(
                            step,
                            &layer_keys,
                            &context.keys[layer],
                            terminal.map(|(key, _)| key),
                        )
                    },
                    |step| {
                        sequence.row(
                            step,
                            &layer_values,
                            &context.values[layer],
                            terminal.map(|(_, value)| value),
                        )
                    },
                    &mut scores,
                    target,
                );
            }
            keys.push(layer_keys);
            values.push(layer_values);
            self.project_attention_into(&attended, block, &mut feed_forward);
            for (slot, &value) in x.iter_mut().zip(&feed_forward.normalized) {
                *slot += value;
            }
            self.feed_forward(&mut x, block, &mut feed_forward);
        }
        let mut hidden = vec![0.0; x.len()];
        layer_norm_into(
            &x,
            &self.final_weight,
            &self.final_bias,
            n_embd,
            &mut hidden,
        );
        BatchStates {
            keys,
            values,
            hidden,
            chains,
        }
    }

    /// The log normalizer `project` computes, for every row of `hidden` (`rows × n_embd`).
    ///
    /// Rows are taken in the same blocks as `linear_into`, so each vocabulary row is widened once
    /// per block instead of once per state, and the logits of a block are kept only until its
    /// normalizers are summed. Each normalizer is computed from bit-identical logits, in the same
    /// order, as `project` would.
    pub(crate) fn log_normalizers_into(&self, hidden: &[f32], out: &mut Vec<f32>) {
        let n_embd = self.config.n_embd;
        let vocab = self.config.vocab;
        let rows = hidden.len() / n_embd;
        out.clear();
        if self.variant.self_normalized {
            out.resize(rows, 0.0);
            return;
        }
        let mut transposed = Vec::new();
        let mut widened = Vec::new();
        let mut logits = Vec::new();
        for (start, size) in row_blocks(rows) {
            #[cfg(test)]
            VOCABULARY_PROJECTIONS.with(|count| count.set(count.get() + size));
            transposed.clear();
            transposed.resize(size * n_embd, 0.0);
            for row in 0..size {
                let source = &hidden[(start + row) * n_embd..(start + row + 1) * n_embd];
                for (input, &value) in source.iter().enumerate() {
                    transposed[input * size + row] = value;
                }
            }
            logits.clear();
            logits.resize(size * vocab, 0.0);
            for index in 0..vocab {
                let span = index * n_embd..(index + 1) * n_embd;
                let (weights, scale): (&[f32], Option<f32>) = match &self.token.inner {
                    Tensor::Float(values) => (&values[span], None),
                    Tensor::Quantized { values, scales } => {
                        widened.clear();
                        widened.extend(values[span].iter().map(|&value| f32::from(value)));
                        (&widened, Some(scales[index]))
                    }
                };
                let mut dots = [0.0f32; ROW_BLOCK];
                match size {
                    ROW_BLOCK => dots = dot_block::<ROW_BLOCK>(&transposed, weights),
                    _ => dots[..HALF_BLOCK]
                        .copy_from_slice(&dot_block::<HALF_BLOCK>(&transposed, weights)),
                }
                for (row, dot) in dots[..size].iter().enumerate() {
                    logits[row * vocab + index] = match scale {
                        Some(scale) => dot * scale,
                        None => *dot,
                    };
                }
            }
            for row in logits.chunks_exact(vocab) {
                out.push(log_normalizer(row));
            }
        }
        let blocked = out.len();
        for row in hidden.chunks_exact(n_embd).skip(blocked) {
            out.push(self.project(row).1);
        }
    }

    pub(crate) fn project_attention_into(
        &self,
        attended: &[f32],
        block: &Block,
        scratch: &mut FeedForwardScratch,
    ) {
        scratch.normalized.resize(attended.len(), 0.0);
        linear_into(
            attended,
            &block.proj_weight,
            &block.proj_bias,
            self.config.n_embd,
            self.config.n_embd,
            &mut scratch.normalized,
            &mut scratch.kernel,
        );
    }

    pub(crate) fn normalize_attention_into(
        &self,
        x: &[f32],
        block: &Block,
        scratch: &mut FeedForwardScratch,
    ) {
        scratch.normalized.resize(x.len(), 0.0);
        layer_norm_into(
            x,
            &block.ln1_weight,
            &block.ln1_bias,
            self.config.n_embd,
            &mut scratch.normalized,
        );
    }

    pub(crate) fn qkv_into(&self, block: &Block, scratch: &mut FeedForwardScratch) {
        let normalized = &scratch.normalized;
        scratch.hidden.resize(normalized.len() * 3, 0.0);
        linear_into(
            normalized,
            &block.qkv_weight,
            &block.qkv_bias,
            self.config.n_embd,
            3 * self.config.n_embd,
            &mut scratch.hidden,
            &mut scratch.kernel,
        );
    }

    pub(crate) fn feed_forward(
        &self,
        x: &mut [f32],
        block: &Block,
        scratch: &mut FeedForwardScratch,
    ) {
        let n_embd = self.config.n_embd;
        scratch.normalized.resize(x.len(), 0.0);
        layer_norm_into(
            x,
            &block.ln2_weight,
            &block.ln2_bias,
            n_embd,
            &mut scratch.normalized,
        );
        scratch.hidden.resize(x.len() * 4, 0.0);
        linear_into(
            &scratch.normalized,
            &block.fc_weight,
            &block.fc_bias,
            n_embd,
            4 * n_embd,
            &mut scratch.hidden,
            &mut scratch.kernel,
        );
        for value in &mut scratch.hidden {
            *value = if self.variant.gelu_tanh {
                gelu_tanh(*value)
            } else {
                gelu(*value)
            };
        }
        // 第一线性层已消费归一化结果，输出投影可以覆盖同一份存储。
        linear_into(
            &scratch.hidden,
            &block.out_weight,
            &block.out_bias,
            4 * n_embd,
            n_embd,
            &mut scratch.normalized,
            &mut scratch.kernel,
        );
        for (slot, &value) in x.iter_mut().zip(&scratch.normalized) {
            *slot += value;
        }
    }

    pub(crate) fn attention(
        &self,
        q: &[f32],
        k: &[f32],
        v: &[f32],
        rows: usize,
        offset: usize,
    ) -> Vec<f32> {
        #[cfg(test)]
        ATTENTION_POSITIONS.with(|count| count.set(count.get() + rows));
        let n_embd = self.config.n_embd;
        let total = k.len() / n_embd;
        let mut out = vec![0.0; rows * n_embd];
        let mut scores = vec![0.0; total];
        for row in 0..rows {
            // Causal: a query at absolute position `offset + row` may attend to itself and earlier.
            self.attend(
                &q[row * n_embd..(row + 1) * n_embd],
                offset + row + 1,
                |step| &k[step * n_embd..(step + 1) * n_embd],
                |step| &v[step * n_embd..(step + 1) * n_embd],
                &mut scores,
                &mut out[row * n_embd..(row + 1) * n_embd],
            );
        }
        out
    }

    /// One query row against the first `visible` positions, every head, added into `target`.
    ///
    /// The positions are reached through `key` and `value` rather than as one contiguous buffer, so
    /// that a batch of candidates can attend to the shared prefix and to its own earlier rows
    /// without first copying them together. The arithmetic and its order are the same either way.
    fn attend<'k>(
        &self,
        query: &[f32],
        visible: usize,
        key: impl Fn(usize) -> &'k [f32],
        value: impl Fn(usize) -> &'k [f32],
        scores: &mut [f32],
        target: &mut [f32],
    ) {
        let heads = self.config.n_head;
        let width = self.config.n_embd / heads;
        let scale = 1.0 / (width as f32).sqrt();
        for head in 0..heads {
            let span = head * width..(head + 1) * width;
            let query = &query[span.clone()];
            let mut highest = f32::NEG_INFINITY;
            for (step, score) in scores[..visible].iter_mut().enumerate() {
                *score = dot(query, &key(step)[span.clone()]) * scale;
                highest = highest.max(*score);
            }
            let mut sum = 0.0;
            for score in &mut scores[..visible] {
                *score = (*score - highest).exp();
                sum += *score;
            }
            let target = &mut target[span.clone()];
            for (step, &score) in scores[..visible].iter().enumerate() {
                let weight = score / sum;
                for (slot, &item) in target.iter_mut().zip(&value(step)[span.clone()]) {
                    *slot += weight * item;
                }
            }
        }
    }
}

/// One position of a batched forward: the token at it, the batch row it follows (`None` for the
/// first position after its context), how many positions after its context it sits, and which of
/// the batch's contexts it continues.
pub(crate) struct BatchRow {
    pub(crate) token: u32,
    pub(crate) parent: Option<usize>,
    pub(crate) depth: usize,
    pub(crate) context: usize,
}

/// What a batch row attends to before its own chain of rows: the first `stored` rows of a run's
/// keys and values, then that run's terminal row when it has one.
#[derive(Clone, Copy)]
pub(crate) struct BatchContext<'a> {
    pub(crate) keys: &'a [Vec<f32>],
    pub(crate) values: &'a [Vec<f32>],
    pub(crate) stored: usize,
    pub(crate) terminal: Option<&'a TerminalState>,
}

impl<'a> BatchContext<'a> {
    pub(crate) fn of(prefix: &'a Prefix<'_>) -> Self {
        Self {
            keys: &prefix.keys,
            values: &prefix.values,
            stored: prefix.length - usize::from(prefix.terminal.is_some()),
            terminal: prefix.terminal,
        }
    }

    pub(crate) fn length(&self) -> usize {
        self.stored + usize::from(self.terminal.is_some())
    }
}

/// The positions one batch row attends to, in order: its context's stored rows, the context's
/// terminal row, then the row's own chain in the batch.
struct Sequence<'c> {
    pub(crate) stored: usize,
    length: usize,
    chain: &'c [usize],
    width: usize,
}

impl Sequence<'_> {
    fn row<'s>(
        &self,
        step: usize,
        batch: &'s [f32],
        stored: &'s [f32],
        terminal: Option<&'s [f32]>,
    ) -> &'s [f32] {
        let width = self.width;
        if step >= self.length {
            let row = self.chain[step - self.length];
            &batch[row * width..(row + 1) * width]
        } else if step < self.stored {
            &stored[step * width..(step + 1) * width]
        } else {
            terminal.expect("a terminal row follows the stored rows")
        }
    }
}

/// What `run_batch` computed: each layer's keys and values for every row (`rows × n_embd`), every
/// row's final normalized state, and every row's chain of batch rows, root first.
pub(crate) struct BatchStates {
    pub(crate) keys: Vec<Vec<f32>>,
    pub(crate) values: Vec<Vec<f32>>,
    pub(crate) hidden: Vec<f32>,
    pub(crate) chains: Vec<Vec<usize>>,
}
