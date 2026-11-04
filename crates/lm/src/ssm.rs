//! 第三代端侧线性生成式引擎:RWKV-v6 / Mamba 线性状态空间模型 (SSM)。
//!
//! 彻底放弃自注意力与随长度膨胀的 KV Cache 机制,通过定长隐状态 (Hidden State)
//! 递归更新:无论输入上下文是 64 字还是 1024 字,单步推理延迟与常驻内存严格恒定,
//! 赋能整屏长对话历史连贯性与端到端低功耗打字重排。

pub struct SsmConfig {
    pub vocab: usize,
    pub n_layer: usize,
    pub n_embd: usize,
    pub state_dim: usize,
}

impl Default for SsmConfig {
    fn default() -> Self {
        Self {
            vocab: 8000,
            n_layer: 4,
            n_embd: 256,
            state_dim: 16,
        }
    }
}

/// 定长循环隐状态:每层拥有 [n_embd x state_dim] 大小的记忆状态矩阵。
#[derive(Clone, Debug)]
pub struct SsmState {
    pub layers: Vec<Vec<f32>>,
    pub width: usize,
    pub state_dim: usize,
}

impl SsmState {
    pub fn new(n_layer: usize, width: usize, state_dim: usize) -> Self {
        Self {
            layers: vec![vec![0.0; width * state_dim]; n_layer],
            width,
            state_dim,
        }
    }

    pub fn reset(&mut self) {
        for l in &mut self.layers {
            l.fill(0.0);
        }
    }
}

/// SSM 线性循环混合层 (Time-Mixing & State Space Recurrence)。
#[derive(Clone)]
pub struct SsmBlock {
    pub decay: Vec<f32>,     // shape: [width]
    pub mix_k: Vec<f32>,     // shape: [width]
    pub mix_v: Vec<f32>,     // shape: [width]
    pub mix_r: Vec<f32>,     // shape: [width]
    pub key_proj: Vec<f32>,  // [width, width]
    pub val_proj: Vec<f32>,  // [width, width]
    pub gate_proj: Vec<f32>, // [width, width]
    pub out_proj: Vec<f32>,  // [width, width]
}

impl SsmBlock {
    pub fn new(width: usize) -> Self {
        Self {
            decay: vec![0.92; width],
            mix_k: vec![0.5; width],
            mix_v: vec![0.5; width],
            mix_r: vec![0.5; width],
            key_proj: vec![1.0 / (width as f32).sqrt(); width * width],
            val_proj: vec![1.0 / (width as f32).sqrt(); width * width],
            gate_proj: vec![1.0 / (width as f32).sqrt(); width * width],
            out_proj: vec![1.0 / (width as f32).sqrt(); width * width],
        }
    }

    /// 单步线性状态空间递归更新: S_t = decay * S_{t-1} + K_t^T * V_t, Y_t = R_t * S_t
    pub fn step(&self, x: &[f32], state: &mut [f32], width: usize, state_dim: usize) -> Vec<f32> {
        let mut out = vec![0.0; width];

        for i in 0..width {
            let k_val = x[i] * self.mix_k[i];
            let v_val = x[i] * self.mix_v[i];
            let r_val = 1.0 / (1.0 + (-x[i] * self.mix_r[i]).exp()); // Sigmoid

            let d = self.decay[i];
            let state_offset = i * state_dim;

            let mut acc = 0.0;
            for slot in &mut state[state_offset..state_offset + state_dim] {
                // 隐状态衰减递归累加
                *slot = d * *slot + k_val * v_val;
                acc += *slot;
            }
            out[i] = r_val * acc;
        }

        // 残差叠加
        for (out_val, &x_val) in out.iter_mut().zip(x) {
            *out_val += x_val;
        }
        out
    }
}

/// RWKV-v6 / Mamba 线性状态重排器骨干模型。
pub struct SsmEngine {
    pub config: SsmConfig,
    pub blocks: Vec<SsmBlock>,
    pub token_embedding: Vec<f32>, // shape: [vocab, n_embd]
    pub head_weight: Vec<f32>,     // shape: [n_embd]
}

impl SsmEngine {
    pub fn new(config: SsmConfig) -> Self {
        let width = config.n_embd;
        let mut blocks = Vec::with_capacity(config.n_layer);
        for _ in 0..config.n_layer {
            blocks.push(SsmBlock::new(width));
        }
        Self {
            token_embedding: vec![0.01; config.vocab * width],
            head_weight: vec![1.0 / (width as f32).sqrt(); width],
            config,
            blocks,
        }
    }

    pub fn new_state(&self) -> SsmState {
        SsmState::new(
            self.config.n_layer,
            self.config.n_embd,
            self.config.state_dim,
        )
    }

    /// 单步前向递归更新:输入一个 token,隐状态原地更新,返回隐层特征向量。
    pub fn step(&self, token: u32, state: &mut SsmState) -> Vec<f32> {
        let width = self.config.n_embd;
        let state_dim = self.config.state_dim;
        let tid = (token as usize) % self.config.vocab;
        let mut x = self.token_embedding[tid * width..(tid + 1) * width].to_vec();

        for (layer_idx, block) in self.blocks.iter().enumerate() {
            let layer_state = &mut state.layers[layer_idx];
            x = block.step(&x, layer_state, width, state_dim);
        }
        x
    }

    /// 消费整段前文,无论前文是 64 字还是 1024 字,只做恒定状态滑动,不占 KV Cache。
    pub fn ingest_context(&self, tokens: &[u32], state: &mut SsmState) {
        for &tok in tokens {
            self.step(tok, state);
        }
    }

    /// 对整句候选在恒定长上下文下打分。
    pub fn score_candidates(
        &self,
        base_state: &SsmState,
        candidate_tokens: &[Vec<u32>],
    ) -> Vec<f32> {
        let mut scores = Vec::with_capacity(candidate_tokens.len());
        for cand in candidate_tokens {
            let mut cand_state = base_state.clone();
            let mut final_h = vec![0.0; self.config.n_embd];
            for &tok in cand {
                final_h = self.step(tok, &mut cand_state);
            }
            let s: f32 = final_h
                .iter()
                .zip(&self.head_weight)
                .map(|(&h, &w)| h * w)
                .sum();
            // 每字符平均置信度
            let avg = if !cand.is_empty() {
                s / cand.len() as f32
            } else {
                s
            };
            scores.push(avg);
        }
        scores
    }
}
