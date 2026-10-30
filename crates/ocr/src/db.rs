//! DB 后处理:概率图 → 文字框。
//!
//! 不做多边形拟合;截图里的文字行基本水平,连通域的轴对齐包围盒已经够用。
//! 阅读顺序按行聚类:先按上边聚成行,再在行内按左边排序。

use crate::Options;

/// 一行文字的最终结果。
#[derive(Clone, Debug, PartialEq)]
pub struct TextBox {
    pub text: String,
    /// det 框置信度与 rec 平均置信度的较小值。
    pub score: f32,
    /// 原图像素坐标:`[左, 上, 右, 下]`。
    pub rect: [f32; 4],
}

/// 检测框;rec 之前只有位置与置信度。
#[derive(Clone, Debug)]
pub(crate) struct DetBox {
    pub(crate) rect: [f32; 4],
    pub(crate) score: f32,
}

/// 概率图坐标换回原图所需的缩放与原图尺寸。
#[derive(Clone, Copy)]
pub(crate) struct Mapping {
    pub(crate) scale_x: f32,
    pub(crate) scale_y: f32,
    pub(crate) image_width: usize,
    pub(crate) image_height: usize,
}

/// 从概率图找文字框;坐标已经换回原图。
pub(crate) fn boxes(
    prob: &[f32],
    width: usize,
    height: usize,
    mapping: Mapping,
    options: &Options,
) -> Vec<DetBox> {
    if width == 0 || height == 0 || prob.len() < width * height {
        return Vec::new();
    }
    let Mapping { scale_x, scale_y, image_width, image_height } = mapping;
    let mut visited = vec![false; width * height];
    let mut found = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..width * height {
        if visited[start] || prob[start] <= options.det_threshold {
            continue;
        }
        stack.push(start);
        visited[start] = true;
        let (mut left, mut right, mut top, mut bottom) = (width, 0usize, height, 0usize);
        let (mut sum, mut count) = (0.0f32, 0usize);
        while let Some(index) = stack.pop() {
            let y = index / width;
            let x = index % width;
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
            sum += prob[index];
            count += 1;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                        continue;
                    }
                    let next = ny as usize * width + nx as usize;
                    if !visited[next] && prob[next] > options.det_threshold {
                        visited[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        if count < 4 {
            continue;
        }
        let score = sum / count as f32;
        if score < options.box_threshold {
            continue;
        }
        // 用包围盒面积与周长估外扩距离,等价于 DB 的 unclip 简化版。
        let box_width = (right - left + 1) as f32;
        let box_height = (bottom - top + 1) as f32;
        let distance =
            box_width * box_height * options.unclip_ratio / (2.0 * (box_width + box_height));
        let rect = [
            ((left as f32 - distance) / scale_x).max(0.0),
            ((top as f32 - distance) / scale_y).max(0.0),
            ((right as f32 + 1.0 + distance) / scale_x).min(image_width as f32),
            ((bottom as f32 + 1.0 + distance) / scale_y).min(image_height as f32),
        ];
        if rect[2] - rect[0] < 2.0 || rect[3] - rect[1] < 2.0 {
            continue;
        }
        found.push(DetBox { rect, score });
    }
    order(found)
}

/// 按行聚类后排序;同一行内从左到右。
fn order(boxes: Vec<DetBox>) -> Vec<DetBox> {
    let mut sorted = boxes;
    sorted.sort_by(|a, b| {
        a.rect[1]
            .total_cmp(&b.rect[1])
            .then(a.rect[0].total_cmp(&b.rect[0]))
    });
    let mut rows: Vec<Vec<DetBox>> = Vec::new();
    for item in sorted {
        let same_row = rows.last().is_some_and(|row| {
            let anchor = &row[0];
            let anchor_height = (anchor.rect[3] - anchor.rect[1]).max(1.0);
            let item_height = (item.rect[3] - item.rect[1]).max(1.0);
            (item.rect[1] - anchor.rect[1]).abs() <= anchor_height.max(item_height) * 0.5
        });
        if same_row && let Some(row) = rows.last_mut() {
            row.push(item);
        } else {
            rows.push(vec![item]);
        }
    }
    let mut out = Vec::new();
    for mut row in rows {
        row.sort_by(|a, b| a.rect[0].total_cmp(&b.rect[0]));
        out.extend(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options::default()
    }

    fn mapping(width: usize, height: usize) -> Mapping {
        Mapping { scale_x: 1.0, scale_y: 1.0, image_width: width, image_height: height }
    }

    #[test]
    fn one_component_becomes_one_box() {
        let mut prob = vec![0.0f32; 40 * 20];
        for y in 5..15 {
            for x in 10..30 {
                prob[y * 40 + x] = 0.9;
            }
        }
        let found = boxes(&prob, 40, 20, mapping(40, 20), &options());
        assert_eq!(found.len(), 1);
        let rect = found[0].rect;
        assert!(rect[0] < 10.0 && rect[2] > 30.0, "{rect:?}");
        assert!(rect[1] < 5.0 && rect[3] > 15.0, "{rect:?}");
        assert!(found[0].score > 0.8);
    }

    #[test]
    fn low_score_and_small_components_are_dropped() {
        let mut prob = vec![0.0f32; 40 * 20];
        prob[2 * 40 + 2] = 0.9;
        for y in 5..15 {
            for x in 10..30 {
                prob[y * 40 + x] = 0.3;
            }
        }
        assert!(boxes(&prob, 40, 20, mapping(40, 20), &options()).is_empty());
    }

    #[test]
    fn rows_are_read_top_to_bottom_and_left_to_right() {
        let mut prob = vec![0.0f32; 80 * 60];
        for (x0, y0) in [(40, 5), (5, 5), (5, 35), (40, 35)] {
            for y in y0..y0 + 10 {
                for x in x0..x0 + 20 {
                    prob[y * 80 + x] = 0.9;
                }
            }
        }
        let found = boxes(&prob, 80, 60, mapping(80, 60), &options());
        assert_eq!(found.len(), 4);
        let xs: Vec<f32> = found.iter().map(|b| b.rect[0]).collect();
        assert!(xs[0] < xs[1] && xs[2] < xs[3], "{xs:?}");
        assert!(found[0].rect[1] < found[2].rect[1]);
    }
}
