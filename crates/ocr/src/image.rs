//! RGB 图的几何变换;只做双线性缩放,和 OpenCV 的 INTER_LINEAR 对齐。

use crate::bmp::RgbImage;

pub(crate) fn resize(image: &RgbImage, width: usize, height: usize) -> Vec<u8> {
    if width == image.width && height == image.height {
        return image.pixels.clone();
    }
    let mut out = vec![0u8; width * height * 3];
    let scale_x = image.width as f32 / width as f32;
    let scale_y = image.height as f32 / height as f32;
    let max_x = (image.width - 1) as f32;
    let max_y = (image.height - 1) as f32;
    for y in 0..height {
        let source_y = ((y as f32 + 0.5) * scale_y - 0.5).clamp(0.0, max_y);
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(image.height - 1);
        let fy = source_y - y0 as f32;
        for x in 0..width {
            let source_x = ((x as f32 + 0.5) * scale_x - 0.5).clamp(0.0, max_x);
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(image.width - 1);
            let fx = source_x - x0 as f32;
            let target = (y * width + x) * 3;
            for channel in 0..3 {
                let top_left = image.pixels[(y0 * image.width + x0) * 3 + channel] as f32;
                let top_right = image.pixels[(y0 * image.width + x1) * 3 + channel] as f32;
                let bottom_left = image.pixels[(y1 * image.width + x0) * 3 + channel] as f32;
                let bottom_right = image.pixels[(y1 * image.width + x1) * 3 + channel] as f32;
                let top = top_left + (top_right - top_left) * fx;
                let bottom = bottom_left + (bottom_right - bottom_left) * fx;
                out[target + channel] = (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: usize, height: usize) -> RgbImage {
        let mut pixels = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[(x * 16) as u8, (y * 16) as u8, 7]);
            }
        }
        RgbImage {
            width,
            height,
            pixels,
        }
    }

    #[test]
    fn same_size_is_identity() {
        let source = image(3, 2);
        assert_eq!(resize(&source, 3, 2), source.pixels);
    }

    #[test]
    fn upscale_keeps_corners() {
        let source = image(2, 2);
        let out = resize(&source, 4, 4);
        assert_eq!(&out[0..3], &[0u8, 0, 7]);
        assert_eq!(&out[out.len() - 3..], &[16u8, 16, 7]);
    }
}
