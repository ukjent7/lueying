//! 动画包:命名的帧序列,平台无关。
//!
//! 动画包由宿主解析成 [`Pack`]:每个片段有一个名字(由皮肤或扩展自己起,如 `idle`、`walk`、
//! `wiggle`),是一串帧;帧号指向平台侧按序号持有的图片;同一片段里每帧时长相同。
//! 行为模型只引用片段的名字,不关心画的是什么。

use std::collections::BTreeMap;

/// 一个片段的帧表。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    /// 帧号,按播放顺序排列;平台侧用它查图片。
    pub frames: Vec<usize>,
    /// 每帧时长(毫秒)。
    pub frame_ms: u64,
}

/// 动画包:片段名字到帧表。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pack {
    clips: BTreeMap<String, Clip>,
}

impl Pack {
    pub fn new(clips: impl IntoIterator<Item = (String, Clip)>) -> Self {
        Self {
            clips: clips.into_iter().collect(),
        }
    }

    /// 按名字取片段;没有这个名字时返回 `None`。
    pub fn clip(&self, name: &str) -> Option<&Clip> {
        self.clips.get(name)
    }
}

/// 解析动画包清单。每行一个片段:`<名字> <每帧毫秒> <帧号>...`。
/// `#` 开头的行与空行忽略;帧号对应动画包目录里的图片文件名(`<帧号>.png`),由宿主去读。
pub fn parse_pack(text: &str) -> Result<Pack, String> {
    let mut clips = BTreeMap::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |message: &str| format!("清单第 {} 行{message}", line_no + 1);
        let mut parts = line.split_whitespace();
        let name = parts.next().ok_or_else(|| at(" 缺少片段名字"))?.to_string();
        let frame_ms: u64 = parts
            .next()
            .and_then(|text| text.parse().ok())
            .filter(|ms| *ms > 0)
            .ok_or_else(|| at(" 每帧毫秒应为正整数"))?;
        let frames = parts
            .map(|text| text.parse::<usize>().map_err(|_| at(" 帧号应为整数")))
            .collect::<Result<Vec<_>, _>>()?;
        if frames.is_empty() {
            return Err(at(" 没有帧号"));
        }
        if clips.insert(name, Clip { frames, frame_ms }).is_some() {
            return Err(at(" 同一片段重复定义"));
        }
    }
    Ok(Pack { clips })
}

/// 某个片段在某时刻应该画的帧号;片段缺失或没有可用的帧时返回 `None`。
pub fn frame_at(name: &str, now_ms: u64, pack: &Pack) -> Option<usize> {
    let clip = pack.clip(name)?;
    if clip.frames.is_empty() || clip.frame_ms == 0 {
        return None;
    }
    let at = (now_ms / clip.frame_ms) as usize % clip.frames.len();
    clip.frames.get(at).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack() -> Pack {
        Pack::new([
            (
                "idle".to_string(),
                Clip {
                    frames: vec![0, 1],
                    frame_ms: 500,
                },
            ),
            (
                "wiggle".to_string(),
                Clip {
                    frames: vec![2, 3, 4],
                    frame_ms: 100,
                },
            ),
        ])
    }

    #[test]
    fn manifest_lines_name_their_clips() {
        let pack = parse_pack("# 小宠物\nidle 500 0 1\n\nmy-dance 100 2\n").expect("清单合法");
        assert_eq!(
            pack.clip("my-dance"),
            Some(&Clip {
                frames: vec![2],
                frame_ms: 100
            })
        );
        assert_eq!(pack.clip("walk"), None);
    }

    #[test]
    fn bad_manifest_lines_are_rejected_with_their_line_number() {
        assert!(parse_pack("idle").unwrap_err().contains("第 1 行"));
        assert!(parse_pack("idle 0 0").unwrap_err().contains("每帧毫秒"));
        assert!(parse_pack("idle 500").unwrap_err().contains("没有帧号"));
        assert!(parse_pack("idle 500 x").unwrap_err().contains("帧号"));
        assert!(
            parse_pack("idle 500 0\nidle 300 1")
                .unwrap_err()
                .contains("重复")
        );
    }

    #[test]
    fn frames_cycle_at_each_clip_rate() {
        let pack = pack();
        assert_eq!(frame_at("idle", 0, &pack), Some(0));
        assert_eq!(frame_at("idle", 500, &pack), Some(1));
        assert_eq!(frame_at("idle", 1_000, &pack), Some(0));
        assert_eq!(frame_at("wiggle", 250, &pack), Some(4));
    }

    #[test]
    fn missing_clips_and_empty_clips_draw_nothing() {
        let pack = pack();
        assert_eq!(frame_at("sleep", 0, &pack), None);
        let empty = Pack::new([(
            "idle".to_string(),
            Clip {
                frames: Vec::new(),
                frame_ms: 500,
            },
        )]);
        assert_eq!(frame_at("idle", 0, &empty), None);
    }
}
