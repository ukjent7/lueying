//! 数值内核:点积、分块线性、层归一化、QKV 拆分与激活函数。
//!
//! 与权重精度有关的分支都收在这里;上层只调 [`Matrix`] 的行点积与
//! `linear_into` 等入口。块化内核的求和顺序与标量参考一致,由测试逐位钉住。

use crate::weights::Tensor;
/// A weight matrix, kept in whatever precision the file stored it in.
///
/// Quantized matrices are not expanded on load. A per-output-row scale is a constant factor for the
/// whole row, so it lifts out of the dot product and multiplies the result once — identical
/// arithmetic, a quarter of the memory. The model runs inside an iOS keyboard extension, where
/// turning a 4.5 MB file into 24.5 MB resident is most of what the process is allowed.
pub(crate) struct Matrix {
    pub(crate) inner: Tensor,
}

impl Matrix {
    pub(crate) fn len(&self) -> usize {
        self.inner.len()
    }

    /// `dot(x, row) * scale[row]`, where the scale is 1 for an unquantized matrix.
    pub(crate) fn dot_row(&self, x: &[f32], row: usize, width: usize) -> f32 {
        let span = row * width..(row + 1) * width;
        match &self.inner {
            Tensor::Float(values) => dot(x, &values[span]),
            Tensor::Quantized { values, scales } => dot_i8(x, &values[span]) * scales[row],
        }
    }

    /// 将 token 与位置嵌入直接写入目标行；量化值先乘行缩放，再加位置。
    pub(crate) fn embed_row(&self, index: usize, position: &[f32], out: &mut [f32]) {
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

/// Independent accumulators rather than one running sum.
///
/// A single accumulator makes every multiply-add wait for the previous one, so the loop runs at the
/// latency of the fused multiply-add instead of its throughput and leaves most of the core idle.
/// Eight partial sums give the pipeline eight independent chains to interleave. The summation order
/// differs from a serial reduction, which is immaterial here: the result feeds a ranking, and the
/// order was never guaranteed to match the trainer's anyway.
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
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
pub(crate) fn linear(
    x: &[f32],
    w: &Matrix,
    bias: &[f32],
    inputs: usize,
    outputs: usize,
) -> Vec<f32> {
    let mut out = vec![0.0; x.len() / inputs * outputs];
    linear_into(
        x,
        w,
        bias,
        inputs,
        outputs,
        &mut out,
        &mut LinearScratch::default(),
    );
    out
}

/// Buffers `linear_into` reuses between calls: the input rows regrouped for the blocked kernel, and
/// one weight row widened to `f32`.
#[derive(Default)]
pub(crate) struct LinearScratch {
    transposed: Vec<f32>,
    row: Vec<f32>,
}

/// `out[m][o] = dot(x[m], w[o]) * scale[o] + bias[o]`, every element bit-for-bit what `dot_row`
/// gives for that row and column.
///
/// Rows are taken eight (or four) at a time against each weight row. A single dot product is bound
/// by the latency of its two accumulator chains and pays for widening every `i8` it reads; a block
/// of rows gives the core eight or sixteen independent chains and widens each weight once for the
/// whole block. That roughly doubles the throughput of the matrix products, which are nearly all of
/// the time a decision takes. Each element still accumulates in the same eight lanes, in the same
/// order, and is reduced the same way as `dot`, so the result does not change by a single bit; the
/// unit tests compare it against the row-at-a-time loop on both quantized and float weights.
///
/// Rows left over after the blocks, fewer than four, go through `dot_row` one at a time, which is
/// as fast as a block for so few rows.
pub(crate) fn linear_into(
    x: &[f32],
    w: &Matrix,
    bias: &[f32],
    inputs: usize,
    outputs: usize,
    out: &mut [f32],
    scratch: &mut LinearScratch,
) {
    let rows = x.len() / inputs;
    let blocks = row_blocks(rows);
    let blocked: usize = blocks.clone().map(|(_, size)| size).sum();
    // Each block is stored input-major, `[input][row]`, so that one weight times the block's rows
    // is a contiguous run.
    scratch.transposed.clear();
    scratch.transposed.resize(blocked * inputs, 0.0);
    for (start, size) in blocks.clone() {
        let target = &mut scratch.transposed[start * inputs..(start + size) * inputs];
        for row in 0..size {
            let source = &x[(start + row) * inputs..(start + row + 1) * inputs];
            for (input, &value) in source.iter().enumerate() {
                target[input * size + row] = value;
            }
        }
    }
    if blocked > 0 {
        for index in 0..outputs {
            let span = index * inputs..(index + 1) * inputs;
            let (weights, scale): (&[f32], Option<f32>) = match &w.inner {
                Tensor::Float(values) => (&values[span], None),
                Tensor::Quantized { values, scales } => {
                    scratch.row.clear();
                    scratch
                        .row
                        .extend(values[span].iter().map(|&value| f32::from(value)));
                    (&scratch.row, Some(scales[index]))
                }
            };
            for (start, size) in blocks.clone() {
                let block = &scratch.transposed[start * inputs..(start + size) * inputs];
                let mut dots = [0.0f32; ROW_BLOCK];
                match size {
                    ROW_BLOCK => dots = dot_block::<ROW_BLOCK>(block, weights),
                    _ => {
                        dots[..HALF_BLOCK].copy_from_slice(&dot_block::<HALF_BLOCK>(block, weights))
                    }
                }
                for (row, dot) in dots[..size].iter().enumerate() {
                    let value = match scale {
                        Some(scale) => dot * scale,
                        None => *dot,
                    };
                    out[(start + row) * outputs + index] = value + bias[index];
                }
            }
        }
    }
    for row in blocked..rows {
        let source = &x[row * inputs..(row + 1) * inputs];
        let target = &mut out[row * outputs..(row + 1) * outputs];
        for (index, slot) in target.iter_mut().enumerate() {
            *slot = w.dot_row(source, index, inputs) + bias[index];
        }
    }
}

/// `highest + ln Σ exp(logit − highest)` over a row of logits, with the maximum and the sum taken
/// in the same order as `SentenceModel::project` takes them.
pub(crate) fn log_normalizer(logits: &[f32]) -> f32 {
    let mut highest = f32::NEG_INFINITY;
    for &logit in logits {
        highest = highest.max(logit);
    }
    let sum: f32 = logits.iter().map(|logit| (logit - highest).exp()).sum();
    highest + sum.ln()
}

/// Rows of input that `linear_into` multiplies against each weight row together.
pub(crate) const ROW_BLOCK: usize = 8;
pub(crate) const HALF_BLOCK: usize = ROW_BLOCK / 2;

/// `(first row, rows)` of each block: as many full blocks as fit, then one half block if four or
/// more rows remain. Whatever is left is fewer than four rows and is not blocked.
pub(crate) fn row_blocks(rows: usize) -> impl Iterator<Item = (usize, usize)> + Clone {
    let full = rows / ROW_BLOCK;
    let half = usize::from(rows % ROW_BLOCK >= HALF_BLOCK);
    (0..full)
        .map(|block| (block * ROW_BLOCK, ROW_BLOCK))
        .chain((0..half).map(move |_| (full * ROW_BLOCK, HALF_BLOCK)))
}

/// `R` dot products of one weight row against `R` input rows stored input-major.
///
/// Row `r` accumulates exactly as `dot` would on its own: lane `l` of eight sums the products at
/// inputs `l, l + 8, l + 16, …` in that order, the lanes are summed first to last, and the inputs
/// past the last multiple of eight are added after that, one at a time. Only the interleaving of
/// independent sums differs, and that interleaving is what lets them run in parallel.
#[inline(never)]
pub(crate) fn dot_block<const R: usize>(block: &[f32], weights: &[f32]) -> [f32; R] {
    const LANES: usize = 8;
    let mut partial = [[0.0f32; R]; LANES];
    for (inputs, weights) in block
        .chunks_exact(LANES * R)
        .zip(weights.chunks_exact(LANES))
    {
        for lane in 0..LANES {
            let weight = weights[lane];
            for row in 0..R {
                partial[lane][row] += inputs[lane * R + row] * weight;
            }
        }
    }
    let tail = weights.len() / LANES * LANES;
    let mut dots = [0.0f32; R];
    for (row, dot) in dots.iter_mut().enumerate() {
        let lanes: [f32; LANES] = std::array::from_fn(|lane| partial[lane][row]);
        let mut total = lanes.iter().sum::<f32>();
        for (input, weight) in weights.iter().enumerate().skip(tail) {
            total += block[input * R + row] * weight;
        }
        *dot = total;
    }
    dots
}

/// The quantized counterpart of `dot`, with the same eight independent accumulators and the same
/// reason for them. The row's scale is applied by the caller, once, rather than to every value:
/// that is the whole point of keeping the weights as `i8`.
pub(crate) fn dot_i8(a: &[f32], b: &[i8]) -> f32 {
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
pub(crate) fn layer_norm(x: &[f32], weight: &[f32], bias: &[f32], width: usize) -> Vec<f32> {
    let mut out = vec![0.0; x.len()];
    layer_norm_into(x, weight, bias, width, &mut out);
    out
}

pub(crate) fn layer_norm_into(
    x: &[f32],
    weight: &[f32],
    bias: &[f32],
    width: usize,
    out: &mut [f32],
) {
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
pub(crate) fn split_qkv(
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
pub(crate) fn split_qkv_with_suffix(
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
pub(crate) fn split_qkv_with_suffix_into(
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
pub(crate) fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + erf(x * std::f32::consts::FRAC_1_SQRT_2))
}

/// `torch.nn.functional.gelu(approximate="tanh")`, used by the 8M keystroke model.
pub(crate) fn gelu_tanh(x: f32) -> f32 {
    0.5 * x * (1.0 + ((2.0 / std::f32::consts::PI).sqrt() * (x + 0.044_715 * x * x * x)).tanh())
}

/// Abramowitz and Stegun 7.1.26, whose error stays below 1.5e-7 across the range.
pub(crate) fn erf(x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_6
            + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    sign * (1.0 - poly * (-x * x).exp())
}
