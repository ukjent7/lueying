//! 候选窗上的小动画:状态表与状态机,平台无关。
//!
//! 动画包由宿主解析成 [`Pack`]:每个状态是一串帧,帧号指向平台侧按序号持有的图片,
//! 同一状态里每帧时长相同。[`Machine`] 只回答两件事:现在是什么状态、现在该画哪一帧。
//! 按键与时间由宿主喂进来,这里不读时钟,便于测试。宠物只是一种动画包,不是特例。

use std::collections::BTreeMap;

/// 一次按键后这段时间内算「打字」。
pub const TYPING_MS: u64 = 1_200;
/// 超过这段时间没有按键就进入「休眠」。
pub const SLEEP_MS: u64 = 60_000;

/// 动画状态。输入法只区分这三种,宠物与其它皮肤共用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Idle,
    Typing,
    Sleep,
}

/// 一个状态的帧表。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    /// 帧号,按播放顺序排列;平台侧用它查图片。
    pub frames: Vec<usize>,
    /// 每帧时长(毫秒)。
    pub frame_ms: u64,
}

/// 动画包:各状态的帧表。缺少的状态回退到待机。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pack {
    clips: BTreeMap<State, Clip>,
}

impl Pack {
    pub fn new(clips: impl IntoIterator<Item = (State, Clip)>) -> Self {
        Self {
            clips: clips.into_iter().collect(),
        }
    }

    /// 某个状态的帧表;该状态缺失时用待机的。
    pub fn clip(&self, state: State) -> Option<&Clip> {
        self.clips
            .get(&state)
            .or_else(|| self.clips.get(&State::Idle))
    }
}

/// 解析动画包清单。每行一个状态:`<状态> <每帧毫秒> <帧号>...`,
/// 状态是 `idle`、`typing`、`sleep` 之一;`#` 开头的行与空行忽略。
/// 帧号对应动画包目录里的图片文件名(`<帧号>.png`),由宿主去读。
pub fn parse_pack(text: &str) -> Result<Pack, String> {
    let mut clips = BTreeMap::new();
    for (line_no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |message: &str| format!("清单第 {} 行{message}", line_no + 1);
        let mut parts = line.split_whitespace();
        let state = match parts.next() {
            Some("idle") => State::Idle,
            Some("typing") => State::Typing,
            Some("sleep") => State::Sleep,
            other => return Err(at(&format!(" 未知状态 {other:?}"))),
        };
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
        if clips.insert(state, Clip { frames, frame_ms }).is_some() {
            return Err(at(" 同一状态重复定义"));
        }
    }
    Ok(Pack { clips })
}

/// 状态机:记住最后一次按键的时间,按空闲时长切换状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    last_key_ms: u64,
}

impl Machine {
    /// 创建时视为刚刚有过输入,从待机开始。
    pub fn new(now_ms: u64) -> Self {
        Self {
            last_key_ms: now_ms,
        }
    }

    /// 有按键时调用。
    pub fn key(&mut self, now_ms: u64) {
        self.last_key_ms = now_ms;
    }

    /// 此刻的状态。
    pub fn state(&self, now_ms: u64) -> State {
        let idle = now_ms.saturating_sub(self.last_key_ms);
        if idle < TYPING_MS {
            State::Typing
        } else if idle < SLEEP_MS {
            State::Idle
        } else {
            State::Sleep
        }
    }

    /// 此刻应该画的帧号;动画包里没有可用的帧时返回 `None`。
    pub fn frame(&self, now_ms: u64, pack: &Pack) -> Option<usize> {
        let clip = pack.clip(self.state(now_ms))?;
        if clip.frames.is_empty() || clip.frame_ms == 0 {
            return None;
        }
        let at = (now_ms / clip.frame_ms) as usize % clip.frames.len();
        clip.frames.get(at).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack() -> Pack {
        Pack::new([
            (
                State::Idle,
                Clip {
                    frames: vec![0, 1],
                    frame_ms: 500,
                },
            ),
            (
                State::Typing,
                Clip {
                    frames: vec![2],
                    frame_ms: 100,
                },
            ),
        ])
    }

    #[test]
    fn keys_switch_to_typing_then_back_to_idle_then_sleep() {
        let mut machine = Machine::new(0);
        assert_eq!(machine.state(0), State::Typing);
        machine.key(10_000);
        assert_eq!(machine.state(10_000 + TYPING_MS - 1), State::Typing);
        assert_eq!(machine.state(10_000 + TYPING_MS), State::Idle);
        assert_eq!(machine.state(10_000 + SLEEP_MS), State::Sleep);
    }

    #[test]
    fn frames_cycle_at_each_clip_rate() {
        let machine = Machine::new(0);
        let pack = pack();
        assert_eq!(machine.frame(TYPING_MS, &pack), Some(0));
        assert_eq!(machine.frame(TYPING_MS + 500, &pack), Some(1));
        assert_eq!(machine.frame(TYPING_MS + 1_000, &pack), Some(0));
    }

    #[test]
    fn manifest_lines_become_clips() {
        let pack =
            parse_pack("# 小宠物\nidle 500 0 1\ntyping 100 2\n\nsleep 1000 3\n").expect("清单合法");
        assert_eq!(
            pack.clip(State::Typing),
            Some(&Clip {
                frames: vec![2],
                frame_ms: 100
            })
        );
        assert_eq!(
            pack.clip(State::Sleep).map(|clip| clip.frames.clone()),
            Some(vec![3])
        );
    }

    #[test]
    fn bad_manifest_lines_are_rejected_with_their_line_number() {
        assert!(parse_pack("walk 500 0").unwrap_err().contains("第 1 行"));
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
    fn missing_state_falls_back_to_idle_and_empty_clips_draw_nothing() {
        let pack = pack();
        assert_eq!(pack.clip(State::Sleep), pack.clip(State::Idle));
        let empty = Pack::new([(
            State::Idle,
            Clip {
                frames: Vec::new(),
                frame_ms: 500,
            },
        )]);
        assert_eq!(Machine::new(0).frame(0, &empty), None);
    }
}
