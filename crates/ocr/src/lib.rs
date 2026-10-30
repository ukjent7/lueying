//! PP-OCRv6 small 的本地推理:det 找文字行,rec 逐行识别。
//!
//! 平台无关:输入是解码好的 RGB 图,输出是按阅读顺序排好的文字行。
//! 模型用纯 Rust 的 tract 跑,不随包带 onnxruntime.dll;空闲时不占线程,
//! 只在 `ocr.recognize` 被调用时进推理。
//!
//! 预处理与后处理照 PaddleOCR 的默认值:det 限长 960、DB 阈值 0.2 /
//! box 阈值 0.45 / unclip 1.4;rec 高 48、宽至少 320、按 CTC 解码。
//! 词表从 `dict.txt` 读,顺序必须与模型输出一致(18708 个字 + 空白 +
//! 空格)。

mod bmp;
mod db;
mod det;
mod dict;
mod image;
mod model;
mod rec;

pub use bmp::{RgbImage, decode_bmp, load_bmp};
pub use db::TextBox;

use std::path::Path;

use tract_onnx::prelude::*;

use model::Plan;

/// 识别参数;默认值对应 PaddleOCR 的 PP-OCRv6 推理配置。
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// det 输入的长边上限;超过就等比缩小。
    pub max_side: usize,
    /// rec 输入的宽度上限;超长行等比压缩。
    pub max_width: usize,
    /// DB 概率图二值化阈值。
    pub det_threshold: f32,
    /// 文字框平均置信度下限。
    pub box_threshold: f32,
    /// 文字框外扩比例,把笔画边缘包进来。
    pub unclip_ratio: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_side: 960,
            max_width: 1280,
            det_threshold: 0.2,
            box_threshold: 0.45,
            unclip_ratio: 1.4,
        }
    }
}

/// 已加载的 PP-OCRv6 模型;`Arc` 共享,推理本身不持有可变状态。
pub struct PpOcr {
    det: Plan,
    rec: Plan,
    dict: Vec<String>,
    options: Options,
}

impl PpOcr {
    /// 从目录加载 `det.onnx`、`rec.onnx`、`dict.txt`。
    ///
    /// 解析与优化都在这里一次完成;首次识别因此会等几秒,之后复用计划。
    pub fn open(dir: &Path) -> Result<Self, String> {
        let det = model::load(&dir.join("det.onnx"))?;
        let rec = model::load(&dir.join("rec.onnx"))?;
        let dict = dict::load(&dir.join("dict.txt"))?;
        Ok(Self {
            det,
            rec,
            dict,
            options: Options::default(),
        })
    }

    /// 换识别参数;模型计划已经按动态尺寸编译,不需要重载。
    pub fn with_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// 识别一张 RGB 图;返回按阅读顺序排好的文字行。
    pub fn recognize(&self, image: &RgbImage) -> Result<Vec<TextBox>, String> {
        if image.width == 0 || image.height == 0 {
            return Err("图片为空".to_string());
        }
        let prepared = det::prepare(image, self.options.max_side);
        let tensor = Tensor::from_shape(&[1, 3, prepared.height, prepared.width], &prepared.data)
            .map_err(|e| format!("det 输入张量失败: {e}"))?;
        let outputs = self
            .det
            .run(tvec!(tensor.into()))
            .map_err(|e| format!("det 推理失败: {e}"))?;
        let output = outputs.first().ok_or_else(|| "det 没有输出".to_string())?;
        let shape = output.shape();
        if shape.len() != 4 || shape[0] != 1 || shape[1] != 1 {
            return Err(format!("det 输出形状异常: {shape:?}"));
        }
        let (map_height, map_width) = (shape[2], shape[3]);
        let prob = output
            .try_as_plain_ram()
            .and_then(|view| view.as_slice::<f32>())
            .map_err(|e| format!("det 输出不是 f32: {e}"))?;
        let boxes = db::boxes(
            prob,
            map_width,
            map_height,
            prepared.scale_x,
            prepared.scale_y,
            image.width,
            image.height,
            &self.options,
        );
        let mut lines = Vec::with_capacity(boxes.len());
        for detected in boxes {
            let crop = rec::prepare(image, &detected.rect, self.options.max_width)?;
            let tensor = Tensor::from_shape(&[1, 3, 48, crop.width], &crop.data)
                .map_err(|e| format!("rec 输入张量失败: {e}"))?;
            let outputs = self
                .rec
                .run(tvec!(tensor.into()))
                .map_err(|e| format!("rec 推理失败: {e}"))?;
            let output = outputs.first().ok_or_else(|| "rec 没有输出".to_string())?;
            let shape = output.shape();
            if shape.len() != 3 || shape[0] != 1 {
                return Err(format!("rec 输出形状异常: {shape:?}"));
            }
            let probs = output
                .try_as_plain_ram()
                .and_then(|view| view.as_slice::<f32>())
                .map_err(|e| format!("rec 输出不是 f32: {e}"))?;
            let (text, score) = rec::decode(probs, shape[1], shape[2], &self.dict)?;
            if text.trim().is_empty() {
                continue;
            }
            lines.push(TextBox {
                text,
                score: detected.score.min(score),
                rect: detected.rect,
            });
        }
        Ok(lines)
    }
}

#[cfg(test)]
#[path = "ocr_tests.rs"]
mod tests;
