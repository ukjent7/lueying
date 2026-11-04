//! 端侧层次化记忆槽 (Hierarchical Memory Slot):基于 HuoziIME 2026 设计。
//!
//! 在 Transformer 输入端提供 16 维连续记忆槽向量,承载用户专属专业术语(如代码标识符、
//! 医学、法律领域术语)的语调风格与领域偏置。全本地滑动更新,零隐私风险,极低算力开销。

pub const MEMORY_DIM: usize = 16;

/// 连续记忆槽状态:16 维可学习/滑动自适应向量。
#[derive(Clone, Debug, PartialEq)]
pub struct MemorySlot {
    pub values: [f32; MEMORY_DIM],
}

impl Default for MemorySlot {
    fn default() -> Self {
        Self {
            values: [0.0; MEMORY_DIM],
        }
    }
}

impl MemorySlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_slice(slice: &[f32]) -> Self {
        let mut values = [0.0; MEMORY_DIM];
        let len = slice.len().min(MEMORY_DIM);
        values[..len].copy_from_slice(&slice[..len]);
        Self { values }
    }

    /// 根据用户最新上屏的文本做梯度外积滑动更新 (EMA 滑动)。
    pub fn update(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let delta = semantic_direction(text);
        const ALPHA: f32 = 0.08;
        for i in 0..MEMORY_DIM {
            self.values[i] = (1.0 - ALPHA) * self.values[i] + ALPHA * delta[i];
        }
    }

    /// 记忆槽范数。
    pub fn norm(&self) -> f32 {
        self.values.iter().map(|v| v * v).sum::<f32>().sqrt()
    }
}

/// 记忆槽向 Transformer 隐层宽度的线性投影偏置。
#[derive(Clone, Debug)]
pub struct MemoryProjection {
    pub weight: Vec<f32>, // shape: [width, MEMORY_DIM]
    pub bias: Vec<f32>,   // shape: [width]
    pub width: usize,
}

impl MemoryProjection {
    pub fn new(width: usize) -> Self {
        // 缺省初始化:轻量恒等/正交基偏置
        let mut weight = vec![0.0; width * MEMORY_DIM];
        for i in 0..width {
            let dim = i % MEMORY_DIM;
            weight[i * MEMORY_DIM + dim] = 1.0 / (MEMORY_DIM as f32).sqrt();
        }
        Self {
            weight,
            bias: vec![0.0; width],
            width,
        }
    }

    /// 把 16 维记忆槽投影为隐层维度的连续引导偏置向量。
    pub fn project(&self, slot: &MemorySlot) -> Vec<f32> {
        let mut out = self.bias.clone();
        for i in 0..self.width {
            let row = &self.weight[i * MEMORY_DIM..(i + 1) * MEMORY_DIM];
            let mut dot = 0.0;
            for j in 0..MEMORY_DIM {
                dot += row[j] * slot.values[j];
            }
            out[i] += dot;
        }
        out
    }
}

/// 从已提交文本提取 16 维归一化语义特征方向向量。
pub fn semantic_direction(text: &str) -> [f32; MEMORY_DIM] {
    let mut vec = [0.0; MEMORY_DIM];
    for (i, ch) in text.chars().enumerate() {
        let h = (ch as u32).wrapping_mul(2654435761).wrapping_add(i as u32);
        let idx = (h as usize) % MEMORY_DIM;
        let sign = if (h & 0x8000) != 0 { 1.0 } else { -1.0 };
        vec[idx] += sign;
    }
    let norm = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 1e-6 {
        for v in &mut vec {
            *v /= norm;
        }
    }
    vec
}
