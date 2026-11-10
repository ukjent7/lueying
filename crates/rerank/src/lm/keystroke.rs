//! 上游按键重排模型(`lm_step.ort`)的加载器。
//!
//! 这些模型与 `sentence-model.safetensors` 的块结构相同,区别只有三处:
//! 文件里的词向量和线性层是 uint8 按张量量化、每层多一个固定的段落向量、候选打分直接
//! 用字向量点积(8M 自归一化,25M 仍需全词表归一化)。因此复用
//! [`SentenceModel`] 的前向与批式评分,只增加一个 `Variant`。
//!
//! 加载时矩阵不展开成 f32:每个输出行重新对称量化成 i8、带一个行缩放(见 `int8_rows`),
//! 这是产品默认的存放精度。位置表与归一化参数仍是 f32。

use std::collections::HashMap;

use serde::Deserialize;

use crate::lm::ModelError;
use crate::lm::math::Matrix;
use crate::lm::model::{Block, Config, SentenceModel, Variant};
use crate::lm::ort::{OrtModel, OrtTensor};
use crate::lm::weights::Tensor;

#[derive(Deserialize)]
struct Manifest {
    layers: usize,
    heads: usize,
    head_dim: usize,
    width: usize,
    max_positions: usize,
    context_chars: usize,
    self_normalized: bool,
    attribution: String,
    license: String,
    config: ManifestConfig,
}

#[derive(Deserialize)]
struct ManifestConfig {
    vocab: usize,
    n_layer: usize,
    n_head: usize,
    n_embd: usize,
    max_positions: usize,
    gelu_tanh: bool,
    self_normalized: bool,
}

#[derive(Deserialize)]
struct VocabFile {
    tokens: Vec<String>,
}

impl SentenceModel {
    /// 从发布的三个文件加载按键重排模型。
    pub fn load_keystroke(
        ort: &[u8],
        manifest_json: &str,
        vocab_json: &str,
    ) -> Result<Self, ModelError> {
        let manifest: Manifest = serde_json::from_str(manifest_json)
            .map_err(|error| ModelError::Header(error.to_string()))?;
        let vocab: VocabFile = serde_json::from_str(vocab_json)
            .map_err(|error| ModelError::Header(error.to_string()))?;
        let config = manifest.config;
        if manifest.layers != config.n_layer
            || manifest.heads != config.n_head
            || manifest.width != config.n_embd
            || manifest.max_positions != config.max_positions
            || manifest.self_normalized != config.self_normalized
            || manifest.head_dim.checked_mul(manifest.heads) != Some(manifest.width)
            || config.vocab != vocab.tokens.len()
            || manifest.context_chars == 0
            || manifest.context_chars > manifest.max_positions
        {
            return Err(ModelError::Geometry("model.json 与模型几何不一致".into()));
        }
        if config.vocab == 0
            || config.n_head == 0
            || config.n_embd == 0
            || !config.n_embd.is_multiple_of(config.n_head)
        {
            return Err(ModelError::Geometry("model.json 的宽度无法分头".into()));
        }

        let index: HashMap<char, u32> = vocab
            .tokens
            .iter()
            .enumerate()
            .filter_map(|(id, token)| {
                let mut chars = token.chars();
                match (chars.next(), chars.next()) {
                    (Some(ch), None) => Some((ch, id as u32)),
                    _ => None,
                }
            })
            .collect();

        let model = OrtModel::parse(ort)?;
        let width = config.n_embd;
        let segment = model
            .weights
            .iter()
            .find_map(|(name, weight)| {
                (name.starts_with("onnx::Add_")
                    && weight.shape.len() == 1
                    && weight.shape[0] == width)
                    .then(|| match &weight.data {
                        OrtTensor::Float(values) => Some(values.clone()),
                        OrtTensor::Uint8(_) => None,
                    })
                    .flatten()
            })
            .ok_or_else(|| ModelError::MissingTensor("segment".into()))?;

        let token = Matrix {
            inner: int8_token(&model, config.vocab, width)?,
        };
        let position = positions(&model, manifest.max_positions, width)?;

        let mut blocks = Vec::with_capacity(config.n_layer);
        for layer in 0..config.n_layer {
            let name = |suffix: &str| format!("model.blocks.{layer}.{suffix}");
            let matrix =
                |kind: &str, outputs: usize, inputs: usize| -> Result<Matrix, ModelError> {
                    let bias = format!("model.blocks.{layer}.{kind}.bias");
                    let base = model
                        .matmuls
                        .get(&bias)
                        .ok_or_else(|| ModelError::MissingTensor(bias.clone()))?;
                    Ok(Matrix {
                        inner: int8_transposed(&model, base, outputs, inputs)?,
                    })
                };
            blocks.push(Block {
                ln1_weight: plain(&model, &name("ln1.weight"))?,
                ln1_bias: plain(&model, &name("ln1.bias"))?,
                qkv_weight: matrix("qkv", 3 * width, width)?,
                qkv_bias: plain(&model, &name("qkv.bias"))?,
                proj_weight: matrix("proj", width, width)?,
                proj_bias: plain(&model, &name("proj.bias"))?,
                ln2_weight: plain(&model, &name("ln2.weight"))?,
                ln2_bias: plain(&model, &name("ln2.bias"))?,
                fc_weight: matrix("mlp.0", 4 * width, width)?,
                fc_bias: plain(&model, &name("mlp.0.bias"))?,
                out_weight: matrix("mlp.2", width, 4 * width)?,
                out_bias: plain(&model, &name("mlp.2.bias"))?,
            });
        }

        let sentence = Self {
            config: Config {
                vocab: config.vocab,
                n_layer: config.n_layer,
                n_head: config.n_head,
                n_embd: config.n_embd,
                context: manifest.max_positions,
            },
            index,
            token,
            position,
            blocks,
            final_weight: plain(&model, "model.ln_f.weight")?,
            final_bias: plain(&model, "model.ln_f.bias")?,
            segment: Some(segment),
            variant: Variant {
                window: manifest.context_chars,
                gelu_tanh: config.gelu_tanh,
                self_normalized: config.self_normalized,
            },
            attribution: manifest.attribution,
            license: manifest.license,
        };
        sentence.check_geometry()?;
        Ok(sentence)
    }
}

fn plain(model: &OrtModel, name: &str) -> Result<Vec<f32>, ModelError> {
    match &model.weight(name)?.data {
        OrtTensor::Float(values) => Ok(values.clone()),
        OrtTensor::Uint8(_) => Err(ModelError::Header(format!("{name} 应为 float32"))),
    }
}

/// 上游按键模型的矩阵是 uint8 按张量量化(一个缩放、一个零点)。加载时把每个矩阵逐行重新对称
/// 量化成 i8,行缩放在点积后乘一次;矩阵只占 f32 的四分之一,激活与累加仍是 f32。
///
/// 量化阵在 ORT 里按 `[输入, 输出]` 存放,转置成线性层要求的 `[输出, 输入]` 行。
fn int8_transposed(
    model: &OrtModel,
    base: &str,
    outputs: usize,
    inputs: usize,
) -> Result<Tensor, ModelError> {
    let source = uint8_source(model, base, &[inputs, outputs], &format!("{base}_scale"))?;
    Ok(int8_rows(&source, outputs, inputs, |output, input| {
        input * outputs + output
    }))
}

/// 词向量表按 `[词表, 宽度]` 存放,每个词一行。
fn int8_token(model: &OrtModel, rows: usize, columns: usize) -> Result<Tensor, ModelError> {
    let base = "model.chars.weight";
    let source = uint8_source(
        model,
        base,
        &[rows, columns],
        &dequant_scale_name(model, base),
    )?;
    Ok(int8_rows(&source, rows, columns, |row, column| {
        row * columns + column
    }))
}

/// 一个 uint8 量化张量的原始码值、张量缩放与零点。
struct Uint8Source<'a> {
    codes: &'a [u8],
    scale: f32,
    zero: f32,
}

fn uint8_source<'a>(
    model: &'a OrtModel,
    base: &str,
    shape: &[usize],
    scale_name: &str,
) -> Result<Uint8Source<'a>, ModelError> {
    let quantized = model.weight(&format!("{base}_quantized"))?;
    if quantized.shape != shape {
        return Err(ModelError::Geometry(format!(
            "{base} 的形状是 {:?},期望 {shape:?}",
            quantized.shape
        )));
    }
    let OrtTensor::Uint8(codes) = &quantized.data else {
        return Err(ModelError::Header(format!("{base} 不是 uint8")));
    };
    Ok(Uint8Source {
        codes,
        scale: scalar_f32(model, scale_name)?,
        zero: scalar_u8(model, &format!("{base}_zero_point"))?,
    })
}

/// 逐行对称量化:行内最大的 |码值 - 零点| 映到 127,行缩放 = 最大值 × 张量缩放 / 127。
/// `at(row, column)` 给出码值在源数组里的下标。整行为零点时任取缩放,码值全零。
fn int8_rows(
    source: &Uint8Source<'_>,
    rows: usize,
    columns: usize,
    at: impl Fn(usize, usize) -> usize,
) -> Tensor {
    let mut values = Vec::with_capacity(rows * columns);
    let mut scales = Vec::with_capacity(rows);
    for row in 0..rows {
        let codes: Vec<f32> = (0..columns)
            .map(|column| f32::from(source.codes[at(row, column)]) - source.zero)
            .collect();
        let largest = codes.iter().fold(0.0f32, |most, code| most.max(code.abs()));
        if largest == 0.0 {
            scales.push(source.scale);
            values.extend(std::iter::repeat_n(0i8, columns));
        } else {
            scales.push(largest * source.scale / 127.0);
            values.extend(
                codes
                    .iter()
                    .map(|code| (code * 127.0 / largest).round() as i8),
            );
        }
    }
    Tensor::Quantized { values, scales }
}

/// 位置表保持 f32:它是加在词向量上的偏置,不进矩阵乘。
fn positions(model: &OrtModel, rows: usize, columns: usize) -> Result<Vec<f32>, ModelError> {
    let base = "model.positions.weight";
    let source = uint8_source(
        model,
        base,
        &[rows, columns],
        &dequant_scale_name(model, base),
    )?;
    Ok(source
        .codes
        .iter()
        .map(|&code| (f32::from(code) - source.zero) * source.scale)
        .collect())
}

/// 优先用与权重量化张量同名的 `_scale`;25M 的字表复用输出投影的缩放。
fn dequant_scale_name(model: &OrtModel, base: &str) -> String {
    let direct = format!("{base}_scale");
    if model.weights.contains_key(&direct) {
        return direct;
    }
    model
        .dequant_scales
        .get(&format!("{base}_zero_point"))
        .cloned()
        .unwrap_or(direct)
}

fn scalar_f32(model: &OrtModel, name: &str) -> Result<f32, ModelError> {
    match &model.weight(name)?.data {
        OrtTensor::Float(values) => values
            .first()
            .copied()
            .ok_or_else(|| ModelError::Geometry(format!("{name} 为空"))),
        OrtTensor::Uint8(_) => Err(ModelError::Header(format!("{name} 应为 float32"))),
    }
}

fn scalar_u8(model: &OrtModel, name: &str) -> Result<f32, ModelError> {
    match &model.weight(name)?.data {
        OrtTensor::Uint8(values) => values
            .first()
            .copied()
            .map(f32::from)
            .ok_or_else(|| ModelError::Geometry(format!("{name} 为空"))),
        OrtTensor::Float(_) => Err(ModelError::Header(format!("{name} 应为 uint8"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{Uint8Source, int8_rows};
    use crate::lm::weights::Tensor;

    #[test]
    fn rows_quantize_around_the_zero_point() {
        // 零点 128:码值 255 与 1 分别是行内最大偏差 127 的两端,全零行取张量缩放。
        let codes = [128u8, 255, 1, 128, 128, 128];
        let source = Uint8Source {
            codes: &codes,
            scale: 0.5,
            zero: 128.0,
        };
        let Tensor::Quantized { values, scales } = int8_rows(&source, 3, 2, |r, c| r * 2 + c)
        else {
            panic!("keystroke matrices must stay quantized");
        };
        assert_eq!(values, vec![0, 127, -127, 0, 0, 0]);
        assert_eq!(scales, vec![0.5, 0.5, 0.5]);
    }
}
