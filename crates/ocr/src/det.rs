//! det 预处理:等比缩放、ImageNet 归一化、BGR/CHW 排布。
//!
//! det 模型的虚拟尺寸在 tract 里推不动(多尺度上采样后接 Concat),
//! 所以输入固定成 960×960 的画布:长边缩到 960、左上角放置,其余用
//! 归一化零值填充。两轴比例分开返回,后处理按各自比例换回原图。

use crate::bmp::RgbImage;
use crate::image;

/// det 模型编译时钉死的输入边长。
pub(crate) const SIDE: usize = 960;

pub(crate) struct DetInput {
    pub(crate) data: Vec<f32>,
    pub(crate) scale_x: f32,
    pub(crate) scale_y: f32,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

pub(crate) fn prepare(image: &RgbImage) -> DetInput {
    let (source_width, source_height) = (image.width, image.height);
    let ratio = (SIDE as f32 / source_width.max(source_height) as f32).min(1.0);
    let width = ((source_width as f32 * ratio).round() as usize).clamp(1, SIDE);
    let height = ((source_height as f32 * ratio).round() as usize).clamp(1, SIDE);
    let resized = image::resize(image, width, height);
    let plane = SIDE * SIDE;
    let mut data = vec![0.0f32; plane * 3];
    // PaddleOCR 的 DecodeImage 用 BGR,归一化均值按 B/G/R 顺序套用。
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    for y in 0..height {
        for x in 0..width {
            let source = (y * width + x) * 3;
            let target = y * SIDE + x;
            let red = resized[source] as f32 / 255.0;
            let green = resized[source + 1] as f32 / 255.0;
            let blue = resized[source + 2] as f32 / 255.0;
            data[target] = (blue - mean[0]) / std[0];
            data[plane + target] = (green - mean[1]) / std[1];
            data[plane * 2 + target] = (red - mean[2]) / std[2];
        }
    }
    DetInput {
        data,
        scale_x: width as f32 / source_width as f32,
        scale_y: height as f32 / source_height as f32,
        width: SIDE,
        height: SIDE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: usize, height: usize) -> RgbImage {
        RgbImage {
            width,
            height,
            pixels: vec![0u8; width * height * 3],
        }
    }

    #[test]
    fn canvas_is_fixed_and_content_is_scaled_down() {
        let input = prepare(&image(1920, 1080));
        assert_eq!((input.width, input.height), (SIDE, SIDE));
        assert!((input.scale_x - 0.5).abs() < 1e-6);
        assert!((input.scale_y - 0.5).abs() < 1e-6);
        assert_eq!(input.data.len(), SIDE * SIDE * 3);
        // 540 行以下都是填充,归一化零值。
        let plane = SIDE * SIDE;
        assert_eq!(input.data[2 * plane + 959 * SIDE + 0], 0.0);
    }

    #[test]
    fn small_image_keeps_scale_one_and_pads() {
        let input = prepare(&image(100, 50));
        assert_eq!((input.width, input.height), (SIDE, SIDE));
        assert!((input.scale_x - 1.0).abs() < 1e-6);
        assert!((input.scale_y - 1.0).abs() < 1e-6);
        let plane = SIDE * SIDE;
        assert_eq!(input.data[500 * SIDE], 0.0);
        assert_eq!(input.data[plane + 50 * SIDE], 0.0);
    }

    #[test]
    fn black_pixels_use_bgr_imagenet_mean() {
        let input = prepare(&image(32, 32));
        let plane = SIDE * SIDE;
        assert!((input.data[0] + 2.1179).abs() < 1e-3);
        assert!((input.data[plane] + 2.0357).abs() < 1e-3);
        assert!((input.data[plane * 2] + 1.8044).abs() < 1e-3);
    }
}
