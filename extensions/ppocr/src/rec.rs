//! rec 预处理与 CTC 解码。
//!
//! 每行文字先裁出来,按高 48 等比缩放;宽度不足 320 时右侧补 0(归一化后
//! 的 0 对应灰底,和 PaddleOCR 的 padding 一致)。解码只取 CTC 头:重复类
//! 合并、空白丢弃,最后一类映射成空格。

use crate::bmp::RgbImage;
use crate::image;

pub(crate) struct RecInput {
    pub(crate) data: Vec<f32>,
    pub(crate) width: usize,
}

pub(crate) fn prepare(
    image: &RgbImage,
    rect: &[f32; 4],
    max_width: usize,
) -> Result<RecInput, String> {
    let left = rect[0].floor().max(0.0) as usize;
    let top = rect[1].floor().max(0.0) as usize;
    let right = rect[2].ceil().clamp(0.0, image.width as f32) as usize;
    let bottom = rect[3].ceil().clamp(0.0, image.height as f32) as usize;
    if right <= left || bottom <= top {
        return Err("文字框为空".to_string());
    }
    let crop = image
        .crop(left, top, right, bottom)
        .ok_or_else(|| "文字框越界".to_string())?;
    let scaled_width = ((48.0 * crop.width as f32 / crop.height as f32).ceil() as usize)
        .clamp(8, max_width.max(8));
    let resized = image::resize(&crop, scaled_width, 48);
    // 与训练一致:短行补到 320,长行保留实际宽度。
    let width = scaled_width.max(320).min(max_width.max(8));
    let plane = 48 * width;
    let mut data = vec![0.0f32; plane * 3];
    for y in 0..48 {
        for x in 0..scaled_width {
            let source = (y * scaled_width + x) * 3;
            let target = y * width + x;
            let red = resized[source] as f32 / 127.5 - 1.0;
            let green = resized[source + 1] as f32 / 127.5 - 1.0;
            let blue = resized[source + 2] as f32 / 127.5 - 1.0;
            data[target] = blue;
            data[plane + target] = green;
            data[plane * 2 + target] = red;
        }
    }
    Ok(RecInput { data, width })
}

/// CTC 解码;返回文本与平均置信度。
pub(crate) fn decode(
    probs: &[f32],
    steps: usize,
    classes: usize,
    dict: &[String],
) -> Result<(String, f32), String> {
    if classes != dict.len() + 2 {
        return Err(format!(
            "词表 {} 行与模型输出 {classes} 类不匹配",
            dict.len()
        ));
    }
    if probs.len() < steps * classes {
        return Err("rec 输出长度不足".to_string());
    }
    let mut text = String::new();
    let mut score = 0.0f32;
    let mut emitted = 0usize;
    let mut previous = usize::MAX;
    for step in 0..steps {
        let row = &probs[step * classes..(step + 1) * classes];
        let mut best = 0usize;
        let mut best_value = row[0];
        for (index, &value) in row.iter().enumerate().skip(1) {
            if value > best_value {
                best = index;
                best_value = value;
            }
        }
        if best != 0 && best != previous {
            if best == dict.len() + 1 {
                text.push(' ');
            } else {
                text.push_str(&dict[best - 1]);
            }
            score += best_value;
            emitted += 1;
        }
        previous = best;
    }
    Ok((
        text,
        if emitted == 0 {
            0.0
        } else {
            score / emitted as f32
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: usize, height: usize) -> RgbImage {
        let mut pixels = Vec::with_capacity(width * height * 3);
        for _ in 0..width * height {
            pixels.extend_from_slice(&[255, 0, 0]);
        }
        RgbImage {
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn short_line_is_padded_to_320() {
        let input = prepare(&image(100, 20), &[0.0, 0.0, 100.0, 20.0], 1280).unwrap();
        assert_eq!(input.width, 320);
        assert_eq!(input.data.len(), 3 * 48 * 320);
        // 红色像素在 BGR 排布里是 (-1, -1, 1)。
        let plane = 48 * 320;
        assert!((input.data[0] + 1.0).abs() < 1e-6);
        assert!((input.data[plane] + 1.0).abs() < 1e-6);
        assert!((input.data[plane * 2] - 1.0).abs() < 1e-6);
        assert_eq!(input.data[300], 0.0);
    }

    #[test]
    fn long_line_keeps_scaled_width() {
        let input = prepare(&image(1000, 20), &[0.0, 0.0, 1000.0, 20.0], 1280).unwrap();
        assert_eq!(input.width, 1280);
    }

    #[test]
    fn ctc_collapses_repeats_and_drops_blanks() {
        let dict = vec!["a".to_string(), "b".to_string()];
        let classes = dict.len() + 2;
        let mut probs = vec![0.0f32; 5 * classes];
        probs[1] = 0.9;
        probs[classes + 2] = 0.9;
        probs[classes * 2 + 2] = 0.8;
        probs[classes * 3] = 0.9;
        probs[classes * 4 + 3] = 0.9;
        let (text, score) = decode(&probs, 5, classes, &dict).unwrap();
        assert_eq!(text, "ab ");
        assert!((score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn dict_mismatch_is_reported() {
        let dict = vec!["a".to_string()];
        assert!(decode(&[0.0; 8], 2, 5, &dict).is_err());
    }
}
