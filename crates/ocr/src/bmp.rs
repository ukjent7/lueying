//! 最小 BMP 解码:截图输出是 24/32 位未压缩 BMP,这里只覆盖这一种。
//!
//! 其它格式(PNG/JPEG)不在这里解;扩展的截图链路固定产出 BMP,外部文件
//! 需要先在系统 OCR 或图像工具里转成 BMP。

use std::path::Path;

/// 解码后的 RGB 图;行优先、自上而下,每像素三个字节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbImage {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl RgbImage {
    /// 取一块矩形(左闭右开);越界部分裁掉。
    pub(crate) fn crop(&self, left: usize, top: usize, right: usize, bottom: usize) -> Option<Self> {
        let right = right.min(self.width);
        let bottom = bottom.min(self.height);
        if left >= right || top >= bottom {
            return None;
        }
        let width = right - left;
        let height = bottom - top;
        let mut pixels = Vec::with_capacity(width * height * 3);
        for y in top..bottom {
            let start = (y * self.width + left) * 3;
            let end = start + width * 3;
            pixels.extend_from_slice(&self.pixels[start..end]);
        }
        Some(Self {
            width,
            height,
            pixels,
        })
    }
}

/// 读 BMP 文件。
pub fn load_bmp(path: &Path) -> Result<RgbImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    decode_bmp(&bytes)
}

/// 解未压缩的 24/32 位 BMP;坏头或截断都返回明确错误。
pub fn decode_bmp(bytes: &[u8]) -> Result<RgbImage, String> {
    if bytes.len() < 54 || &bytes[..2] != b"BM" {
        return Err("不是 BMP 文件".to_string());
    }
    let offset = u32_at(bytes, 10).ok_or_else(|| "BMP 头不完整".to_string())? as usize;
    let header = u32_at(bytes, 14).ok_or_else(|| "BMP 头不完整".to_string())? as usize;
    if header < 40 {
        return Err(format!("不支持的 BMP 头长度 {header}"));
    }
    let width = i32_at(bytes, 18).ok_or_else(|| "BMP 宽度缺失".to_string())?;
    let height = i32_at(bytes, 22).ok_or_else(|| "BMP 高度缺失".to_string())?;
    let bits = u16_at(bytes, 28).ok_or_else(|| "BMP 位深缺失".to_string())?;
    let compression = u32_at(bytes, 30).ok_or_else(|| "BMP 压缩方式缺失".to_string())?;
    if compression != 0 {
        return Err(format!("只支持未压缩 BMP(压缩方式 {compression})"));
    }
    if bits != 24 && bits != 32 {
        return Err(format!("只支持 24/32 位 BMP(当前 {bits} 位)"));
    }
    if width <= 0 || height == 0 {
        return Err(format!("BMP 尺寸无效({width}x{height})"));
    }
    let width = width as usize;
    let height = height.unsigned_abs() as usize;
    let Some(count) = width.checked_mul(height) else {
        return Err("BMP 尺寸溢出".to_string());
    };
    if count > 100_000_000 {
        return Err(format!("BMP 过大({width}x{height})"));
    }
    let stride = (width * (bits as usize / 8) + 3) / 4 * 4;
    let needed = offset
        .checked_add(stride.checked_mul(height).ok_or("BMP 尺寸溢出")?)
        .ok_or("BMP 尺寸溢出")?;
    if bytes.len() < needed {
        return Err(format!(
            "BMP 数据不完整(需要 {needed} 字节,实际 {})",
            bytes.len()
        ));
    }
    let top_down = height < 0;
    let mut out = vec![0u8; count * 3];
    for y in 0..height {
        let source_y = if top_down { y } else { height - 1 - y };
        let row = offset + source_y * stride;
        for x in 0..width {
            let source = row + x * (bits as usize / 8);
            let target = (y * width + x) * 3;
            out[target] = bytes[source + 2];
            out[target + 1] = bytes[source + 1];
            out[target + 2] = bytes[source];
        }
    }
    Ok(RgbImage {
        width,
        height,
        pixels: out,
    })
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn i32_at(bytes: &[u8], at: usize) -> Option<i32> {
    u32_at(bytes, at).map(|value| value as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bmp(width: i32, height: i32, bits: u16, top_down: bool, pixels: &[[u8; 3]]) -> Vec<u8> {
        let stride = (width as usize * (bits as usize / 8) + 3) / 4 * 4;
        let mut out = vec![0u8; 54 + stride * height.unsigned_abs() as usize];
        let size = out.len() as u32;
        out[0..2].copy_from_slice(b"BM");
        out[2..6].copy_from_slice(&size.to_le_bytes());
        out[10..14].copy_from_slice(&54u32.to_le_bytes());
        out[14..18].copy_from_slice(&40u32.to_le_bytes());
        out[18..22].copy_from_slice(&width.to_le_bytes());
        out[22..26].copy_from_slice(&(if top_down { -height } else { height }).to_le_bytes());
        out[26..28].copy_from_slice(&1u16.to_le_bytes());
        out[28..30].copy_from_slice(&bits.to_le_bytes());
        for (index, rgb) in pixels.iter().enumerate() {
            let x = index % width as usize;
            let y = index / width as usize;
            let row = if top_down { y } else { height as usize - 1 - y };
            let at = 54 + row * stride + x * (bits as usize / 8);
            out[at] = rgb[2];
            out[at + 1] = rgb[1];
            out[at + 2] = rgb[0];
        }
        out
    }

    #[test]
    fn decodes_bottom_up_24_bit() {
        let data = bmp(2, 2, 24, false, &[[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 12]]);
        let image = decode_bmp(&data).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(image.pixels, vec![7, 8, 9, 10, 11, 12, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn decodes_top_down_32_bit() {
        let data = bmp(1, 2, 32, true, &[[1, 2, 3], [4, 5, 6]]);
        let image = decode_bmp(&data).unwrap();
        assert_eq!(image.pixels, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn rejects_compressed_and_truncated() {
        let mut data = bmp(1, 1, 24, false, &[[0, 0, 0]]);
        data[30..34].copy_from_slice(&1u32.to_le_bytes());
        assert!(decode_bmp(&data).is_err());
        let mut short = bmp(2, 2, 24, false, &[[0, 0, 0]; 4]);
        short.truncate(60);
        assert!(decode_bmp(&short).is_err());
    }

    #[test]
    fn crop_keeps_rows_and_clips() {
        let image = RgbImage {
            width: 3,
            height: 2,
            pixels: vec![0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4, 4, 5, 5, 5],
        };
        let crop = image.crop(1, 0, 9, 9).unwrap();
        assert_eq!((crop.width, crop.height), (2, 2));
        assert_eq!(crop.pixels, vec![1, 1, 1, 2, 2, 2, 4, 4, 4, 5, 5, 5]);
        assert!(image.crop(2, 0, 2, 1).is_none());
    }
}
