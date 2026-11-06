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
