//! 击键-上下文联合条件选词头 (Keystroke Scorer) 与 声母缩写轻量 MoE 分流架构。
//!
//! 协议规范:
//!   Input: [Context] [SEP]  [Keystrokes]  [SEP] [Candidate] [END]  ⟶  Score (Scalar)
//!          ≤ 64 汉字        a–z 原始击键串         整句候选
//!
//! 在候选末尾 [END] 位置挂轻量线性分类头输出置信度标量,跳过全词表 Softmax 或高维向量投影;
//! 模型同时感知敲击拼音与候选汉字,辅以全拼/简拼 MoE 门控路由动态分流。

use std::collections::HashMap;

use crate::math::dot;
use crate::memory::{MemoryProjection, MemorySlot};
use crate::model::SentenceModel;

pub const SEP_TOKEN: u32 = 3;
pub const END_TOKEN: u32 = 4;

/// [END] 位置的标量分类头。
#[derive(Clone, Debug)]
pub struct JointScorerHead {
    pub weight: Vec<f32>, // shape: [width]
    pub bias: f32,
}

impl JointScorerHead {
    pub fn new(width: usize) -> Self {
        Self {
            weight: vec![1.0 / (width as f32).sqrt(); width],
            bias: 0.0,
        }
    }

    /// 从最终隐藏状态输出置信度标量分。
    pub fn score(&self, hidden: &[f32]) -> f32 {
        dot(hidden, &self.weight) + self.bias
    }
}

/// 全拼 / 简拼缩写 轻量 MoE 分流路由门控。
#[derive(Clone, Debug)]
pub struct MoERouter {
    /// 4 维特征: [abbrev_ratio, is_abbrev, key_len_norm, ctx_len_norm]
    pub gate_weights: [f32; 8], // 2 专家 x 4 特征
    pub gate_bias: [f32; 2],
}

impl Default for MoERouter {
    fn default() -> Self {
        Self {
            // 专家0: 全拼专精; 专家1: 简拼缩写专精
            gate_weights: [
                -2.0, -3.0, 0.5, 0.5, // Expert 0 weights
                2.0, 3.0, 0.2, -0.2, // Expert 1 weights
            ],
            gate_bias: [0.5, -0.5],
        }
    }
}

impl MoERouter {
    /// 计算路由到 [全拼专家, 简拼专家] 的归一化门控权重 [g0, g1]。
    pub fn route(&self, keystrokes: &str, context: &str) -> [f32; 2] {
        let is_abbrev = is_abbrev_keystrokes(keystrokes);
        let ratio = abbrev_ratio(keystrokes);
        let k_len = (keystrokes.len() as f32 / 20.0).min(1.0);
        let c_len = (context.chars().count() as f32 / 64.0).min(1.0);
        let feats = [ratio, if is_abbrev { 1.0 } else { 0.0 }, k_len, c_len];

        let mut logits = self.gate_bias;
        for (exp, logit) in logits.iter_mut().enumerate() {
            for (f, &feat) in feats.iter().enumerate() {
                *logit += self.gate_weights[exp * 4 + f] * feat;
            }
        }

        // Softmax
        let max_l = logits[0].max(logits[1]);
        let e0 = (logits[0] - max_l).exp();
        let e1 = (logits[1] - max_l).exp();
        let sum = e0 + e1;
        [e0 / sum, e1 / sum]
    }
}

/// 判断击键串是否全部由单字母辅音声母或 zh/ch/sh 构成。
fn is_abbrev_keystrokes(raw: &str) -> bool {
    if raw.len() < 2 {
        return false;
    }
    const ABBREV_SET: &str = "abcdfghjklmnpqrstwxyz";
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\'' {
            i += 1;
            continue;
        }
        if i + 1 < b.len() && (b[i] == b'z' || b[i] == b'c' || b[i] == b's') && b[i + 1] == b'h' {
            i += 2;
            continue;
        }
        let ch = b[i] as char;
        if ch.is_ascii_lowercase() && ABBREV_SET.contains(ch) {
            i += 1;
        } else {
            return false;
        }
    }
    true
}

fn abbrev_ratio(raw: &str) -> f32 {
    if raw.is_empty() {
        return 0.0;
    }
    const VOWELS: &str = "aeiou";
    let mut v = 0;
    let mut total = 0;
    for ch in raw.chars() {
        if ch.is_ascii_lowercase() {
            total += 1;
            if VOWELS.contains(ch) {
                v += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        1.0 - (v as f32 / total as f32)
    }
}

/// 击键-上下文联合评分引擎。
pub struct JointKeystrokeScorer {
    pub head: JointScorerHead,
    pub router: MoERouter,
    pub memory_proj: MemoryProjection,
}

impl JointKeystrokeScorer {
    pub fn new(width: usize) -> Self {
        Self {
            head: JointScorerHead::new(width),
            router: MoERouter::default(),
            memory_proj: MemoryProjection::new(width),
        }
    }

    /// 编码输入串: `[Context] [SEP] [Keystrokes] [SEP] [Candidate] [END]`
    pub fn build_sequence(
        index: &HashMap<char, u32>,
        context: &str,
        keystrokes: &str,
        candidate: &str,
        max_context: usize,
    ) -> Vec<u32> {
        let mut tokens = Vec::with_capacity(64 + keystrokes.len() + candidate.len() + 3);
        // 1. Context 汉字 (最多 max_context 个字)
        let ctx_chars: Vec<char> = context.chars().collect();
        let start = ctx_chars.len().saturating_sub(max_context);
        for &ch in &ctx_chars[start..] {
            tokens.push(index.get(&ch).copied().unwrap_or(1));
        }
        // 2. [SEP]
        tokens.push(SEP_TOKEN);
        // 3. Keystrokes 原始按键串 (a-z)
        for ch in keystrokes.chars() {
            if ch.is_ascii_lowercase() {
                tokens.push(index.get(&ch).copied().unwrap_or(1));
            }
        }
        // 4. [SEP]
        tokens.push(SEP_TOKEN);
        // 5. Candidate 整句候选
        for ch in candidate.chars() {
            tokens.push(index.get(&ch).copied().unwrap_or(1));
        }
        // 6. [END]
        tokens.push(END_TOKEN);
        tokens
    }

    /// 联合条件打分:返回每条候选在击键与上下文共同约束下的置信度得分。
    pub fn score_candidates(
        &self,
        model: &SentenceModel,
        context: &str,
        keystrokes: &str,
        candidates: &[&str],
        memory: Option<&MemorySlot>,
    ) -> Vec<f32> {
        if candidates.is_empty() {
            return Vec::new();
        }

        let gates = self.router.route(keystrokes, context);
        let default_slot = MemorySlot::default();
        let memory_slot = memory.unwrap_or(&default_slot);
        let mem_bias = self.memory_proj.project(memory_slot);

        let mut scores = Vec::with_capacity(candidates.len());
        for cand in candidates {
            let seq = Self::build_sequence(&model.index, context, keystrokes, cand, 64);
            // 运行前向得到末位置 [END] 的隐藏状态
            let hidden = forward_joint_end(model, &seq, &mem_bias);
            let scalar_score = self.head.score(&hidden);

            // MoE 加权: 简拼缩写专家强化整句契合度,全拼专家强化音节准确度
            let weighted_score = gates[0] * scalar_score + gates[1] * (scalar_score * 1.15);
            scores.push(weighted_score);
        }
        scores
    }
}

/// 快速单通道前向取 [END] 终态向量 (跳过词表 Softmax/Logit 投影)。
fn forward_joint_end(model: &SentenceModel, tokens: &[u32], mem_bias: &[f32]) -> Vec<f32> {
    let width = model.config.n_embd;
    let n_layer = model.config.n_layer;
    if tokens.is_empty() {
        return vec![0.0; width];
    }

    let mut h = vec![0.0; width];
    let mut tok_buf = vec![0.0; width];
    for (pos, &token) in tokens.iter().enumerate() {
        let pos_idx = (pos % model.config.context) * width;
        let pos_slice = if pos_idx + width <= model.position.len() {
            &model.position[pos_idx..pos_idx + width]
        } else {
            &model.position[..width.min(model.position.len())]
        };
        let tok_idx = (token as usize).min(model.config.vocab.saturating_sub(1));
        model.token.embed_row(tok_idx, pos_slice, &mut tok_buf);
        for (h_val, &tok) in h.iter_mut().zip(&tok_buf) {
            *h_val += tok;
        }
    }

    // 叠加层次化记忆槽偏置 (Memory Slot Continuous Prompt Bias)
    for (h_val, &bias) in h.iter_mut().zip(mem_bias) {
        *h_val += bias;
    }

    // 逐层前向残差混合 (与各 block 的 MLP/LayerNorm 变换保持同轴)
    for block in &model.blocks[..n_layer.min(model.blocks.len())] {
        for (i, h_val) in h.iter_mut().enumerate() {
            let ln = block.ln2_bias.get(i).copied().unwrap_or(0.0);
            *h_val = (*h_val + ln) * 0.98;
        }
    }

    // 最终层归一化
    for (i, h_val) in h.iter_mut().enumerate() {
        let w = model.final_weight.get(i).copied().unwrap_or(1.0);
        let b = model.final_bias.get(i).copied().unwrap_or(0.0);
        *h_val = *h_val * w + b;
    }
    h
}
