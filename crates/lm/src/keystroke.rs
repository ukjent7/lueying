//! 上游按键重排模型(`lm_step.ort`)的加载器。
//!
//! 这些模型与 `sentence-model.safetensors` 的块结构相同,区别只有三处:
//! 词向量和位置表是 uint8 量化、每层多一个固定的段落向量、候选打分直接
//! 用字向量点积(8M 自归一化,25M 仍需全词表归一化)。因此复用
//! [`SentenceModel`] 的前向与批式评分,只增加一个 `Variant`。

use std::collections::HashMap;

use serde::Deserialize;

use crate::ort::{OrtModel, OrtTensor};
use crate::weights::Tensor;
use crate::{Block, Config, Matrix, ModelError, SentenceModel, Variant};

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
            inner: Tensor::Float(dequantize_rows(
                &model,
                "model.chars.weight",
                config.vocab,
                width,
            )?),
        };
        let position = dequantize_rows(
            &model,
            "model.positions.weight",
            manifest.max_positions,
            width,
        )?;

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
                        inner: Tensor::Float(dequantize_transposed(&model, base, outputs, inputs)?),
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

/// 量化阵在 ORT 里按 `[输入, 输出]` 存放,换成线性层要求的 `[输出, 输入]`。
fn dequantize_transposed(
    model: &OrtModel,
    base: &str,
    outputs: usize,
    inputs: usize,
) -> Result<Vec<f32>, ModelError> {
    let quantized = model.weight(&format!("{base}_quantized"))?;
    if quantized.shape != vec![inputs, outputs] {
        return Err(ModelError::Geometry(format!(
            "{base} 的形状是 {:?},期望 [{inputs}, {outputs}]",
            quantized.shape
        )));
    }
    let scale = scalar_f32(model, &format!("{base}_scale"))?;
    let zero = scalar_u8(model, &format!("{base}_zero_point"))?;
    let OrtTensor::Uint8(values) = &quantized.data else {
        return Err(ModelError::Header(format!("{base} 不是 uint8")));
    };
    let mut out = vec![0.0; outputs * inputs];
    for (output, row) in out.chunks_exact_mut(inputs).enumerate() {
        for (input, slot) in row.iter_mut().enumerate() {
            *slot = (f32::from(values[input * outputs + output]) - zero) * scale;
        }
    }
    Ok(out)
}

fn dequantize_rows(
    model: &OrtModel,
    base: &str,
    rows: usize,
    columns: usize,
) -> Result<Vec<f32>, ModelError> {
    let quantized = model.weight(&format!("{base}_quantized"))?;
    if quantized.shape != vec![rows, columns] {
        return Err(ModelError::Geometry(format!(
            "{base} 的形状是 {:?},期望 [{rows}, {columns}]",
            quantized.shape
        )));
    }
    let scale = scalar_f32(model, &dequant_scale_name(model, base))?;
    let zero = scalar_u8(model, &format!("{base}_zero_point"))?;
    let OrtTensor::Uint8(values) = &quantized.data else {
        return Err(ModelError::Header(format!("{base} 不是 uint8")));
    };
    Ok(values
        .iter()
        .map(|&value| (f32::from(value) - zero) * scale)
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
