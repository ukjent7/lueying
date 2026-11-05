//! 整句模型:权重加载、几何校验、编码与评分入口。
//!
//! 逐层前向与批式评分在 [`crate::forward`],数值内核在 [`crate::math`];
//! 这里只负责把权重文件变成模型,再把模型用到候选上。

use std::collections::HashMap;

use crate::ModelError;
#[cfg(test)]
use crate::forward::Prefix;
use crate::forward::{BatchContext, BatchRow, HiddenState};
use crate::math::{LinearScratch, Matrix, dot, dot_i8};
use crate::weights::{Tensor, Weights};
#[cfg(test)]
use crate::{FORWARD_POSITIONS, LAST_ENCODED_LENGTH, LAST_ENCODED_POINTER, VOCABULARY_PROJECTIONS};
const UNK: u32 = 1;
pub(crate) const BOS: u32 = 2;

#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub(crate) struct Config {
    pub(crate) vocab: usize,
    pub(crate) n_layer: usize,
    pub(crate) n_head: usize,
    pub(crate) n_embd: usize,
    pub(crate) context: usize,
}

pub(crate) struct Block {
    pub(crate) ln1_weight: Vec<f32>,
    pub(crate) ln1_bias: Vec<f32>,
    pub(crate) qkv_weight: Matrix,
    pub(crate) qkv_bias: Vec<f32>,
    pub(crate) proj_weight: Matrix,
    pub(crate) proj_bias: Vec<f32>,
    pub(crate) ln2_weight: Vec<f32>,
    pub(crate) ln2_bias: Vec<f32>,
    pub(crate) fc_weight: Matrix,
    pub(crate) fc_bias: Vec<f32>,
    pub(crate) out_weight: Matrix,
    pub(crate) out_bias: Vec<f32>,
}

#[derive(Default)]
pub(crate) struct FeedForwardScratch {
    pub(crate) normalized: Vec<f32>,
    pub(crate) hidden: Vec<f32>,
    pub(crate) kernel: LinearScratch,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Variant {
    /// 训练时使用的上文窗口;0 表示与 `Config::context` 相同。
    pub(crate) window: usize,
    /// 上游 8M 按键模型用 tanh 近似的 GELU,整句模型用精确 erf。
    pub(crate) gelu_tanh: bool,
    /// 8M 按键模型自归一化,候选分数只算一次点积。
    pub(crate) self_normalized: bool,
}

pub struct SentenceModel {
    pub(crate) config: Config,
    pub(crate) index: HashMap<char, u32>,
    pub(crate) token: Matrix,
    pub(crate) position: Vec<f32>,
    pub(crate) blocks: Vec<Block>,
    pub(crate) final_weight: Vec<f32>,
    pub(crate) final_bias: Vec<f32>,
    /// 按键模型在每段嵌入上加的段落向量;整句模型为 `None`。
    pub(crate) segment: Option<Vec<f32>>,
    pub(crate) variant: Variant,
    pub(crate) attribution: String,
    pub(crate) license: String,
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

impl SentenceModel {
    pub fn load(bytes: &[u8]) -> Result<Self, ModelError> {
        let mut weights = Weights::parse(bytes)?;
        let config: Config = serde_json::from_str(weights.metadata("config")?)
            .map_err(|error| ModelError::Header(error.to_string()))?;
        let vocabulary: Vec<String> = serde_json::from_str(weights.metadata("vocab")?)
            .map_err(|error| ModelError::Header(error.to_string()))?;
        let attribution = weights
            .metadata("attribution")
            .unwrap_or_default()
            .to_owned();
        let license = weights.metadata("license").unwrap_or_default().to_owned();

        // Every dimension below is multiplied out while checking the tensors, and all of them come
        // from the file. Bounding them here means the later arithmetic cannot overflow, which is
        // simpler to be sure of than checking each multiplication.
        const LARGEST: usize = 1 << 20;
        if config.vocab > LARGEST
            || config.n_layer > LARGEST
            || config.n_embd > LARGEST
            || config.context > LARGEST
        {
            return Err(ModelError::Geometry(
                "a dimension is larger than any real model".into(),
            ));
        }
        // `%` rather than `is_multiple_of`, which needs a far newer compiler than anything else here.
        if config.n_head == 0 || config.n_embd == 0 || !config.n_embd.is_multiple_of(config.n_head)
        {
            return Err(ModelError::Geometry(format!(
                "{} channels do not divide into {} heads",
                config.n_embd, config.n_head
            )));
        }
        if vocabulary.len() != config.vocab {
            return Err(ModelError::Geometry(format!(
                "vocabulary holds {} tokens but the configuration says {}",
                vocabulary.len(),
                config.vocab
            )));
        }

        let index = vocabulary
            .iter()
            .enumerate()
            .filter_map(|(id, token)| {
                let mut chars = token.chars();
                match (chars.next(), chars.next()) {
                    // Reserved tokens are spelled `<pad>` and are addressed by constant, not by text.
                    (Some(ch), None) => Some((ch, id as u32)),
                    _ => None,
                }
            })
            .collect();

        let mut blocks = Vec::with_capacity(config.n_layer);
        for layer in 0..config.n_layer {
            let name = |suffix: &str| format!("blocks.{layer}.{suffix}");
            blocks.push(Block {
                ln1_weight: weights.take(&name("ln1.weight"))?,
                ln1_bias: weights.take(&name("ln1.bias"))?,
                qkv_weight: Matrix {
                    inner: weights.take_tensor(&name("qkv.weight"))?,
                },
                qkv_bias: weights.take(&name("qkv.bias"))?,
                proj_weight: Matrix {
                    inner: weights.take_tensor(&name("proj.weight"))?,
                },
                proj_bias: weights.take(&name("proj.bias"))?,
                ln2_weight: weights.take(&name("ln2.weight"))?,
                ln2_bias: weights.take(&name("ln2.bias"))?,
                fc_weight: Matrix {
                    inner: weights.take_tensor(&name("fc.weight"))?,
                },
                fc_bias: weights.take(&name("fc.bias"))?,
                out_weight: Matrix {
                    inner: weights.take_tensor(&name("out.weight"))?,
                },
                out_bias: weights.take(&name("out.bias"))?,
            });
        }

        let model = Self {
            config,
            index,
            token: Matrix {
                inner: weights.take_tensor("tok.weight")?,
            },
            position: weights.take("pos.weight")?,
            blocks,
            final_weight: weights.take("ln_f.weight")?,
            final_bias: weights.take("ln_f.bias")?,
            segment: None,
            variant: Variant::default(),
            attribution,
            license,
        };
        model.check_geometry()?;
        Ok(model)
    }

    pub(crate) fn check_geometry(&self) -> Result<(), ModelError> {
        let Config {
            vocab,
            n_embd,
            context,
            ..
        } = self.config;
        let expect = |name: &str, actual: usize, wanted: usize| {
            (actual == wanted).then_some(()).ok_or_else(|| {
                ModelError::Geometry(format!("{name} holds {actual}, wanted {wanted}"))
            })
        };
        let expect_matrix = |name: &str, matrix: &Matrix, rows: usize, columns: usize| {
            expect(name, matrix.len(), rows * columns)?;
            if let Tensor::Quantized { scales, .. } = &matrix.inner {
                expect(&format!("{name}.scale"), scales.len(), rows)?;
            }
            Ok(())
        };
        expect_matrix("tok.weight", &self.token, vocab, n_embd)?;
        expect("pos.weight", self.position.len(), context * n_embd)?;
        expect("ln_f.weight", self.final_weight.len(), n_embd)?;
        if let Some(segment) = &self.segment {
            expect("segment", segment.len(), n_embd)?;
        }
        for (layer, block) in self.blocks.iter().enumerate() {
            expect_matrix(
                &format!("blocks.{layer}.qkv"),
                &block.qkv_weight,
                3 * n_embd,
                n_embd,
            )?;
            expect_matrix(
                &format!("blocks.{layer}.proj"),
                &block.proj_weight,
                n_embd,
                n_embd,
            )?;
            expect_matrix(
                &format!("blocks.{layer}.fc"),
                &block.fc_weight,
                4 * n_embd,
                n_embd,
            )?;
            expect_matrix(
                &format!("blocks.{layer}.out"),
                &block.out_weight,
                n_embd,
                4 * n_embd,
            )?;
        }
        Ok(())
    }

    /// The corpus attribution carried in the weights file. Both corpora require it to travel with a
    /// model trained on them, which is why it lives in the file rather than beside it.
    pub fn attribution(&self) -> &str {
        &self.attribution
    }

    /// The licence of the **weights**, as an SPDX identifier, which is not the licence of the code
    /// that produced them. Empty when the file does not say — older files do not, and a caller that
    /// needs to know should treat silence as unknown rather than as permission.
    pub fn license(&self) -> &str {
        &self.license
    }

    pub fn context_length(&self) -> usize {
        self.window()
    }

    fn window(&self) -> usize {
        if self.variant.window == 0 {
            self.config.context
        } else {
            self.variant.window
        }
    }

    pub(crate) fn encode(&self, text: &str) -> Vec<u32> {
        self.encode_limited(text, usize::MAX)
    }

    /// 模型不会评分窗口之后的 token，编码和保留容量都限制在这个窗口内。
    pub(crate) fn encode_limited(&self, text: &str, limit: usize) -> Vec<u32> {
        let tokens: Vec<u32> = text
            .chars()
            .take(limit)
            .map(|ch| self.index.get(&ch).copied().unwrap_or(UNK))
            .collect();
        #[cfg(test)]
        LAST_ENCODED_POINTER.with(|pointer| pointer.set(tokens.as_ptr() as usize));
        #[cfg(test)]
        LAST_ENCODED_LENGTH.with(|length| length.set(tokens.len()));
        tokens
    }

    /// Mean log-probability per character of each candidate, conditioned on the committed text.
    ///
    /// The mean rather than the sum, because candidates of different lengths are otherwise not
    /// comparable. Callers are expected to pass only candidates that cover the whole key, which
    /// makes the lengths equal in practice; normalizing anyway means a caller that gets that wrong
    /// gets a degraded ranking rather than a meaningless one.
    pub fn score(&self, context: &str, candidates: &[&str]) -> Vec<f32> {
        if candidates.is_empty() {
            return Vec::new();
        }
        let longest = candidates
            .iter()
            .map(|text| text.chars().count())
            .max()
            .unwrap_or(0);
        let prefix = self.run_prefix(&self.prefix_tokens(context, longest));
        let limit = self.config.context.saturating_sub(prefix.length).max(1);
        let tails: Vec<Vec<u32>> = candidates
            .iter()
            .map(|text| self.encode_limited(text, limit))
            .collect();

        // Candidates of one decision usually share their first characters, and a position's state
        // depends only on the tokens up to it, so each distinct prefix of a candidate is run once.
        // `steps[c][i]` is the batch row whose state predicts token `i + 1` of candidate `c`. The
        // last token of a candidate predicts nothing and is never run.
        let mut rows: Vec<BatchRow> = Vec::new();
        let mut known: HashMap<(usize, u32), usize> = HashMap::new();
        let steps: Vec<Vec<usize>> = tails
            .iter()
            .map(|tail| {
                let mut parent = None;
                let mut path = Vec::with_capacity(tail.len().saturating_sub(1));
                for (depth, &token) in tail.iter().enumerate().take(tail.len().saturating_sub(1)) {
                    let key = (parent.map_or(0, |row| row + 1), token);
                    let row = *known.entry(key).or_insert_with(|| {
                        rows.push(BatchRow {
                            token,
                            parent,
                            depth,
                            context: 0,
                        });
                        rows.len() - 1
                    });
                    path.push(row);
                    parent = Some(row);
                }
                path
            })
            .collect();
        let hidden = self.run_batch(&[BatchContext::of(&prefix)], &rows).hidden;
        let mut normalizers = Vec::with_capacity(rows.len());
        self.log_normalizers_into(&hidden, &mut normalizers);

        let n_embd = self.config.n_embd;
        tails
            .iter()
            .zip(&steps)
            .map(|(tail, path)| {
                let Some(&first) = tail.first() else {
                    return f32::NEG_INFINITY;
                };
                // The same additions in the same order as `run_tail`, so the sum is the same f32.
                let mut running = self.log_probability(&prefix.last_hidden, first);
                for (&row, &next) in path.iter().zip(&tail[1..]) {
                    let next = (next as usize).min(self.config.vocab - 1);
                    running +=
                        self.token
                            .dot_row(&hidden[row * n_embd..(row + 1) * n_embd], next, n_embd)
                            - normalizers[row];
                }
                running / tail.len() as f32
            })
            .collect()
    }

    /// The committed text as tokens, opened with `BOS` and trimmed to leave room for the longest
    /// candidate.
    ///
    /// The position table is the hard limit on sequence length, so the context yields rather than
    /// the candidate: a truncated candidate would be scored on text the user did not type, whereas
    /// a truncated context only weakens the conditioning.
    pub(crate) fn prefix_tokens(&self, context: &str, longest: usize) -> Vec<u32> {
        // 整句模型把候选也算进 context 总长;按键模型的 context_chars 只限制上文,
        // 候选长度由位置表 capacity 另行限制,与上游 `write_golden` 的切法一致。
        let room = if self.variant.window == 0 {
            self.config.context.saturating_sub(longest + 1)
        } else {
            self.variant.window
        };
        // 词表外的字符(常见的是上一句结尾的标点)对条件没有信息量,只会把
        // 上文结尾变成 <unk>;剥掉它们,上游横评里 25M 的整句有上文 top-1
        // 从 87.0% 抬到 89.9%(metasequoiaime/chinese-ime-lm #91,sentences-ctx-v1)。
        let encoded: Vec<u32> = self
            .encode(context)
            .into_iter()
            .filter(|&token| token != UNK)
            .collect();
        let start = encoded.len().saturating_sub(room);
        let mut tokens = Vec::with_capacity(encoded.len() - start + 1);
        tokens.push(BOS);
        tokens.extend_from_slice(&encoded[start..]);
        tokens
    }

    /// Scores each candidate on its own against the prefix: what `score` did before it ran shared
    /// prefixes once, kept so the tests can hold the batched path to the same bits.
    #[cfg(test)]
    pub(crate) fn score_against(&self, prefix: &Prefix<'_>, candidates: &[&str]) -> Vec<f32> {
        candidates
            .iter()
            .map(|text| {
                let tail = self.encode(text);
                if tail.is_empty() {
                    return f32::NEG_INFINITY;
                }
                let limit = self.config.context.saturating_sub(prefix.length).max(1);
                let tail = &tail[..tail.len().min(limit)];
                self.score_tail(prefix, tail) / tail.len() as f32
            })
            .collect()
    }

    pub(crate) fn embed(&self, tokens: &[u32], offset: usize) -> Vec<f32> {
        #[cfg(test)]
        FORWARD_POSITIONS.with(|count| count.set(count.get() + tokens.len()));
        let n_embd = self.config.n_embd;
        let mut x = vec![0.0; tokens.len() * n_embd];
        for (step, &token) in tokens.iter().enumerate() {
            let token = (token as usize).min(self.config.vocab - 1);
            let position = (offset + step).min(self.config.context - 1);
            let row = &mut x[step * n_embd..(step + 1) * n_embd];
            let position_row = &self.position[position * n_embd..(position + 1) * n_embd];
            self.token.embed_row(token, position_row, row);
            if let Some(segment) = &self.segment {
                for (slot, &value) in row.iter_mut().zip(segment) {
                    *slot += value;
                }
            }
        }
        x
    }

    /// 同一隐藏状态的完整词表只投影一次，后续 token 只计算自己的 logit。
    pub(crate) fn log_probability(&self, hidden: &HiddenState, token: u32) -> f32 {
        let token = (token as usize).min(self.config.vocab - 1);
        if self.variant.self_normalized {
            return self
                .token
                .dot_row(&hidden.values, token, self.config.n_embd);
        }
        let offset = hidden
            .log_normalizer
            .get_or_init(|| self.project(&hidden.values).1);
        self.token
            .dot_row(&hidden.values, token, self.config.n_embd)
            - offset
    }

    /// 全词表 logits 与归一化常数保持既有的点积、最高值和指数求和顺序。
    pub(crate) fn project(&self, hidden: &[f32]) -> (Vec<f32>, f32) {
        #[cfg(test)]
        VOCABULARY_PROJECTIONS.with(|count| count.set(count.get() + 1));
        let n_embd = self.config.n_embd;
        let mut logits = Vec::with_capacity(self.config.vocab);
        let mut highest = f32::NEG_INFINITY;
        match &self.token.inner {
            Tensor::Float(values) => {
                for row in values.chunks_exact(n_embd).take(self.config.vocab) {
                    let logit = dot(hidden, row);
                    highest = highest.max(logit);
                    logits.push(logit);
                }
            }
            Tensor::Quantized { values, scales } => {
                for (row, &scale) in values
                    .chunks_exact(n_embd)
                    .zip(&scales[..self.config.vocab])
                    .take(self.config.vocab)
                {
                    let logit = dot_i8(hidden, row) * scale;
                    highest = highest.max(logit);
                    logits.push(logit);
                }
            }
        }
        let sum: f32 = logits.iter().map(|logit| (logit - highest).exp()).sum();
        let offset = highest + sum.ln();
        (logits, offset)
    }

    /// 返回每个 token 的对数概率，公开的完整分布入口仍计算所有行。
    #[cfg(test)]
    pub(crate) fn distribution(&self, hidden: &[f32]) -> Vec<f32> {
        let (mut logits, offset) = self.project(hidden);
        for logit in &mut logits {
            *logit -= offset;
        }
        logits
    }

    /// The identifier this model uses for `character`, when it has one.
    pub fn token(&self, character: char) -> Option<u32> {
        self.index.get(&character).copied()
    }
}
