//! PP-OCRv6 原生扩展:det 找文字行,rec 逐行识别,独立进程常驻。
//!
//! 平台无关:输入是 BMP 解码出的 RGB 图,输出是按阅读顺序排好的文字行。
//! 模型用纯 Rust 的 tract 跑,不随包带 onnxruntime.dll。进程由宿主的
//! 原生扩展宿主拉起:计划与词表在启动时加载,之后常驻到空闲回收,模型
//! 与可执行文件同目录(det.onnx、rec.onnx、dict.txt)。
//!
//! 协议与旁路层一致:一行 `{"method":"recognize","args":{"file":...}}`,
//! 一行 `{"result":{"text":...,"lang":"ppocr"}}` 或 `{"error":"..."}`。
//!
//! 预处理与后处理照 PaddleOCR 的默认值:det 固定 960×960 画布、DB 阈值
//! 0.2 / box 阈值 0.45 / unclip 1.4;rec 高 48、宽至少 320、按 CTC 解码。
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

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use tract_onnx::prelude::*;

use model::Plan;

fn main() {
    // 模型在可执行文件旁边;启动即加载,失败则每个请求回同一错误。
    let model = model_dir().map(|dir| PpOcr::open(&dir));
    let stdin = std::io::stdin();
    for line in BufReader::new(stdin.lock()).lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<serde_json::Value>(&line) else {
            break;
        };
        let reply = match request["method"].as_str() {
            Some("recognize") => recognize(&model, request["args"]["file"].as_str()),
            Some(method) => Err(format!("没有方法 {method:?}")),
            None => Err("缺 method".to_string()),
        };
        match reply {
            Ok(value) => println!("{{\"result\":{value}}}"),
            Err(error) => println!(
                "{{\"error\":{}}}",
                serde_json::to_string(&error).unwrap_or_else(|_| "\"内部错误\"".to_string())
            ),
        }
        let _ = std::io::stdout().flush();
    }
}

/// 模型目录 = 可执行文件所在目录;沙箱对它有只读授权。
fn model_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// 识别一张 BMP;`model` 是启动时缓存的加载结果。
fn recognize(model: &Option<Result<PpOcr, String>>, file: Option<&str>) -> Result<String, String> {
    let Some(file) = file else {
        return Err("缺 file".to_string());
    };
    let model = match model {
        Some(Ok(model)) => model,
        Some(Err(error)) => return Err(error.clone()),
        None => return Err("模型目录不可解析:可执行文件路径异常".to_string()),
    };
    let image = load_bmp(Path::new(file))?;
    let lines = model.recognize(&image)?;
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::to_string(&serde_json::json!({ "text": text, "lang": "ppocr" }))
        .map_err(|e| e.to_string())
}

/// 识别参数;默认值对应 PaddleOCR 的 PP-OCRv6 推理配置。
#[derive(Clone, Copy, Debug)]
pub struct Options {
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
        let det = model::load(&dir.join("det.onnx"), Some(&[1, 3, det::SIDE, det::SIDE]))?;
        let rec = model::load(&dir.join("rec.onnx"), None)?;
        let dict = dict::load(&dir.join("dict.txt"))?;
        Ok(Self {
            det,
            rec,
            dict,
            options: Options::default(),
        })
    }

    /// 识别一张 RGB 图;返回按阅读顺序排好的文字行。
    pub fn recognize(&self, image: &RgbImage) -> Result<Vec<TextBox>, String> {
        if image.width == 0 || image.height == 0 {
            return Err("图片为空".to_string());
        }
        let prepared = det::prepare(image);
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
            db::Mapping {
                scale_x: prepared.scale_x,
                scale_y: prepared.scale_y,
                image_width: image.width,
                image_height: image.height,
            },
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
