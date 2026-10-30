//! det 预处理:等比缩放、ImageNet 归一化、BGR/CHW 排布。
//!
//! 缩放后的宽高各自取 32 的倍数(四舍五入),与 PaddleOCR 的
//! `DetResizeForTest` 一致;两轴比例分开返回,后处理按各自比例换回原图。

use crate::bmp::RgbImage;
use crate::image;

pub(crate) struct DetInput {
    pub(crate) data: Vec<f32>,
    pub(crate) scale_x: f32,
    pub(crate) scale_y: f32,
    pub(crate) width: usize,
    pub(crate) height: usize,
}

pub(crate) fn prepare(image: &RgbImage, max_side: usize) -> DetInput {
    let (source_width, source_height) = (image.width, image.height);
    let longest = source_width.max(source_height);
    let ratio = if longest > max_side {
        max_side as f32 / longest as f32
    } else {
        1.0
    };
    let width = (((source_width as f32 * ratio) / 32.0).round() as usize * 32).max(32);
    let height = (((source_height as f32 * ratio) / 32.0).round() as usize * 32).max(32);
    let resized = image::resize(image, width, height);
    let plane = width * height;
    let mut data = vec![0.0f32; plane * 3];
    // PaddleOCR 的 DecodeImage 用 BGR,归一化均值按 B/G/R 顺序套用。
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    for index in 0..plane {
        let red = resized[index * 3] as f32 / 255.0;
        let green = resized[index * 3 + 1] as f32 / 255.0;
        let blue = resized[index * 3 + 2] as f32 / 255.0;
        data[index] = (blue - mean[0]) / std[0];
        data[plane + index] = (green - mean[1]) / std[1];
        data[plane * 2 + index] = (red - mean[2]) / std[2];
    }
    DetInput {
        data,
        scale_x: width as f32 / source_width as f32,
        scale_y: height as f32 / source_height as f32,
        width,
        height,
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
    fn long_side_is_limited_and_rounded_to_32() {
        let input = prepare(&image(1920, 1080), 960);
        assert_eq!((input.width, input.height), (960, 544));
        assert!((input.scale_x - 0.5).abs() < 1e-6);
        assert!((input.scale_y - 544.0 / 1080.0).abs() < 1e-6);
        assert_eq!(input.data.len(), 960 * 544 * 3);
    }

    #[test]
    fn small_image_is_upscaled_to_the_grid() {
        let input = prepare(&image(100, 50), 960);
        assert_eq!((input.width, input.height), (96, 64));
    }

    #[test]
    fn black_pixels_use_bgr_imagenet_mean() {
        let input = prepare(&image(32, 32), 960);
        assert!((input.data[0] + 2.1179).abs() < 1e-3);
        assert!((input.data[32 * 32] + 2.0357).abs() < 1e-3);
        assert!((input.data[32 * 32 * 2] + 1.8044).abs() < 1e-3);
    }
}
