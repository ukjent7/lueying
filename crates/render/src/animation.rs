//! 动画包:命名的帧序列,平台无关。
//!
//! 动画包由宿主解析成 [`Pack`](Windows 端见 `skin_pack`):每个片段有一个名字(由皮肤或扩展自己起,如 `idle`、`walk`、
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

    /// 所有片段里最大的帧号;宿主据此知道要读多少张图。没有任何帧时返回 `None`。
    pub fn max_frame(&self) -> Option<usize> {
        self.clips
            .values()
            .flat_map(|clip| clip.frames.iter().copied())
            .max()
    }
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
    fn clips_are_found_by_name() {
        let pack = pack();
        assert_eq!(pack.clip("wiggle").map(|clip| clip.frame_ms), Some(100));
        assert_eq!(pack.clip("walk"), None);
        assert_eq!(pack.max_frame(), Some(4));
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
