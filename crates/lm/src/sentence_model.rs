//! `SentenceModel` 的前向、前缀缓存与评分实现。
//!
//! 类型定义留在 crate 根,这里只放 inherent impl;根模块的私有字段和辅助
//! 类型对子模块可见,拆文件不需要把它们改成公开 API。

use super::*;
use crate::weights::Weights;

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
        if config.n_head == 0 || config.n_embd == 0 || config.n_embd % config.n_head != 0 {
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
        self.config.context
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
        self.score_against(&prefix, candidates)
    }

    /// The committed text as tokens, opened with `BOS` and trimmed to leave room for the longest
    /// candidate.
    ///
    /// The position table is the hard limit on sequence length, so the context yields rather than
    /// the candidate: a truncated candidate would be scored on text the user did not type, whereas
    /// a truncated context only weakens the conditioning.
    pub(crate) fn prefix_tokens(&self, context: &str, longest: usize) -> Vec<u32> {
        let room = self.config.context.saturating_sub(longest + 1);
        let encoded = self.encode(context);
        let start = encoded.len().saturating_sub(room);
        let mut tokens = Vec::with_capacity(encoded.len() - start + 1);
        tokens.push(BOS);
        tokens.extend_from_slice(&encoded[start..]);
        tokens
    }

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
        }
        x
    }

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
    pub(crate) fn score_tail(&self, prefix: &Prefix<'_>, tail: &[u32]) -> f32 {
        let run = self.run_tail(prefix, tail);
        run.cumulative[run.cumulative.len() - 1]
    }

    /// 完整累计分只需要前 length-1 个隐藏状态；末位置留给真正经过它的续跑。
    ///
    /// 分段累计仍沿用既有分组：carried + rest，不改变 token 的先后顺序。
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
        );
    }

    pub(crate) fn normalize_attention_into(&self, x: &[f32], block: &Block, scratch: &mut FeedForwardScratch) {
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
        );
    }

    pub(crate) fn feed_forward(&self, x: &mut [f32], block: &Block, scratch: &mut FeedForwardScratch) {
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
        );
        for value in &mut scratch.hidden {
            *value = gelu(*value);
        }
        // 第一线性层已消费归一化结果，输出投影可以覆盖同一份存储。
        linear_into(
            &scratch.hidden,
            &block.out_weight,
            &block.out_bias,
            4 * n_embd,
            n_embd,
            &mut scratch.normalized,
        );
        for (slot, &value) in x.iter_mut().zip(&scratch.normalized) {
            *slot += value;
        }
    }

    /// 同一隐藏状态的完整词表只投影一次，后续 token 只计算自己的 logit。
    pub(crate) fn log_probability(&self, hidden: &HiddenState, token: u32) -> f32 {
        let token = (token as usize).min(self.config.vocab - 1);
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
    pub(crate) fn distribution(&self, hidden: &[f32]) -> Vec<f32> {
        let (mut logits, offset) = self.project(hidden);
        for logit in &mut logits {
            *logit -= offset;
        }
        logits
    }

    /// log P(next character | text), over the whole vocabulary.
    ///
    /// This is the primitive a search needs: reranking asks the model to judge finished strings,
    /// while a search asks it what should come next, which is what lets it reach a reading the
    /// engine never proposed.
    pub fn next_log_probabilities(&self, text: &str) -> Vec<f32> {
        let mut tokens = vec![BOS];
        let encoded = self.encode(text);
        let room = self.config.context - 1;
        tokens.extend_from_slice(&encoded[encoded.len().saturating_sub(room)..]);
        let prefix = self.run_prefix(&tokens);
        self.distribution(&prefix.last_hidden.values)
    }

    /// The identifier this model uses for `character`, when it has one.
    pub fn token(&self, character: char) -> Option<u32> {
        self.index.get(&character).copied()
    }

    pub(crate) fn attention(&self, q: &[f32], k: &[f32], v: &[f32], rows: usize, offset: usize) -> Vec<f32> {
        #[cfg(test)]
        ATTENTION_POSITIONS.with(|count| count.set(count.get() + rows));
        let n_embd = self.config.n_embd;
        let heads = self.config.n_head;
        let width = n_embd / heads;
        let scale = 1.0 / (width as f32).sqrt();
        let total = k.len() / n_embd;
        let mut out = vec![0.0; rows * n_embd];
        let mut scores = vec![0.0; total];
        for row in 0..rows {
            // Causal: a query at absolute position `offset + row` may attend to itself and earlier.
            let visible = offset + row + 1;
            for head in 0..heads {
                let base = head * width;
                let query = &q[row * n_embd + base..row * n_embd + base + width];
                let mut highest = f32::NEG_INFINITY;
                for (step, score) in scores[..visible].iter_mut().enumerate() {
                    let key = &k[step * n_embd + base..step * n_embd + base + width];
                    *score = dot(query, key) * scale;
                    highest = highest.max(*score);
                }
                let mut sum = 0.0;
                for score in &mut scores[..visible] {
                    *score = (*score - highest).exp();
                    sum += *score;
                }
                let target = &mut out[row * n_embd + base..row * n_embd + base + width];
                for (step, &score) in scores[..visible].iter().enumerate() {
                    let weight = score / sum;
                    let value = &v[step * n_embd + base..step * n_embd + base + width];
                    for (slot, &item) in target.iter_mut().zip(value) {
                        *slot += weight * item;
                    }
                }
            }
        }
        out
    }
}
