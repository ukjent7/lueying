//! 候选窗横带里的小宠物:自己走、玩、睡,碰到边会转身,位置不固定。平台无关。
//!
//! 宠物只回答三件事:现在是什么状态、朝哪边、在横带的哪个位置。画面由宿主按 [`View`]
//! 去取动画帧(见 `animation::frame_at`)。按键与时间由宿主喂进来,这里不读时钟;
//! 随机数是固定种子的 xorshift,同一种子同样的行为,便于测试。

use crate::animation::{SLEEP_MS, State, TYPING_MS};

/// 走路速度(像素/秒)。
const SPEED: f32 = 28.0;
/// 各种行为的持续时间范围(毫秒)。
const WALK_MS: (u64, u64) = (2_000, 6_000);
const PLAY_MS: (u64, u64) = (1_500, 3_000);
const IDLE_MS: (u64, u64) = (1_000, 4_000);

/// 朝向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facing {
    Left,
    Right,
}

/// 宿主需要的全部信息:状态、横带里的位置(左缘,像素)、朝向。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub state: State,
    pub x: f32,
    pub facing: Facing,
}

/// 固定种子的伪随机数(xorshift64),不引入依赖。
#[derive(Clone, Copy, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// `lo..=hi` 之间的一个数。
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }
}

/// 宠物。
#[derive(Clone, Debug)]
pub struct Pet {
    x: f32,
    facing: Facing,
    state: State,
    /// 当前这段行为结束的时刻。
    until_ms: u64,
    last_ms: u64,
    last_key_ms: u64,
    rng: Rng,
}

impl Pet {
    /// 创建时视为刚刚有过输入,从待机开始。
    pub fn new(seed: u64, now_ms: u64) -> Self {
        Self {
            x: 0.0,
            facing: Facing::Right,
            state: State::Idle,
            until_ms: now_ms,
            last_ms: now_ms,
            last_key_ms: now_ms,
            rng: Rng::new(seed),
        }
    }

    /// 有按键时调用:宠物会停下来看着输入。
    pub fn key(&mut self, now_ms: u64) {
        self.last_key_ms = now_ms;
    }

    /// 推进到 `now_ms`。`area` 是横带宽度,`sprite` 是宠物宽度(都是像素)。
    pub fn tick(&mut self, now_ms: u64, area: f32, sprite: f32) -> View {
        let max_x = (area - sprite).max(0.0);
        let dt = now_ms.saturating_sub(self.last_ms) as f32 / 1000.0;
        self.last_ms = now_ms;
        let quiet = now_ms.saturating_sub(self.last_key_ms);
        if quiet < TYPING_MS {
            self.state = State::Typing;
        } else if quiet >= SLEEP_MS {
            self.state = State::Sleep;
        } else if matches!(self.state, State::Typing | State::Sleep) || now_ms >= self.until_ms {
            self.choose(now_ms);
        }
        if self.state == State::Walk {
            self.step(dt, max_x);
        }
        self.x = self.x.clamp(0.0, max_x);
        View {
            state: self.state,
            x: self.x,
            facing: self.facing,
        }
    }

    /// 从一段空闲里挑下一段行为:走、玩或待机。
    fn choose(&mut self, now_ms: u64) {
        let (state, span) = match self.rng.range(0, 99) {
            0..=49 => (State::Walk, WALK_MS),
            50..=69 => (State::Play, PLAY_MS),
            _ => (State::Idle, IDLE_MS),
        };
        self.state = state;
        self.until_ms = now_ms + self.rng.range(span.0, span.1);
        self.facing = if self.rng.range(0, 1) == 0 {
            Facing::Left
        } else {
            Facing::Right
        };
    }

    /// 走一小步;碰到横带两端就停在端点并转身。
    fn step(&mut self, dt: f32, max_x: f32) {
        let dir = match self.facing {
            Facing::Left => -1.0,
            Facing::Right => 1.0,
        };
        let next = self.x + dir * SPEED * dt;
        if next >= max_x {
            self.x = max_x;
            self.facing = Facing::Left;
        } else if next <= 0.0 {
            self.x = 0.0;
            self.facing = Facing::Right;
        } else {
            self.x = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每 50 毫秒推进一次,跑 `total_ms` 毫秒,返回每一步的视图。
    fn run(pet: &mut Pet, from_ms: u64, total_ms: u64, area: f32, sprite: f32) -> Vec<View> {
        (from_ms..from_ms + total_ms)
            .step_by(50)
            .map(|now| pet.tick(now, area, sprite))
            .collect()
    }

    #[test]
    fn stays_inside_the_strip_and_turns_at_both_ends() {
        let (area, sprite) = (300.0, 48.0);
        let max_x = area - sprite;
        let mut pet = Pet::new(7, 0);
        let views = run(&mut pet, 0, 600_000, area, sprite);
        for view in &views {
            assert!((0.0..=max_x).contains(&view.x), "越界: {}", view.x);
            if view.state == State::Walk && view.x >= max_x - 0.001 {
                assert_eq!(view.facing, Facing::Left);
            }
            if view.state == State::Walk && view.x <= 0.001 {
                assert_eq!(view.facing, Facing::Right);
            }
        }
        assert!(views.iter().any(|view| view.state == State::Walk));
        assert!(views.iter().any(|view| view.x > max_x / 2.0));
    }

    #[test]
    fn typing_keeps_the_pet_still_and_watching() {
        let mut pet = Pet::new(3, 0);
        pet.key(0);
        let views = run(&mut pet, 0, TYPING_MS - 50, 300.0, 48.0);
        assert!(views.iter().all(|view| view.state == State::Typing));
        assert!(views.windows(2).all(|pair| pair[0].x == pair[1].x));
    }

    #[test]
    fn falls_asleep_after_a_minute_and_wakes_on_a_key() {
        let mut pet = Pet::new(3, 0);
        pet.key(0);
        let before = run(&mut pet, 0, SLEEP_MS - 50, 300.0, 48.0);
        assert!(before.iter().all(|view| view.state != State::Sleep));
        let asleep = run(&mut pet, SLEEP_MS, 30_000, 300.0, 48.0);
        assert!(asleep.iter().all(|view| view.state == State::Sleep));
        pet.key(SLEEP_MS + 30_000);
        assert_eq!(
            pet.tick(SLEEP_MS + 30_050, 300.0, 48.0).state,
            State::Typing
        );
    }

    #[test]
    fn same_seed_gives_the_same_story() {
        let mut first = Pet::new(42, 0);
        let mut second = Pet::new(42, 0);
        assert_eq!(
            run(&mut first, 0, 120_000, 300.0, 48.0),
            run(&mut second, 0, 120_000, 300.0, 48.0)
        );
    }

    #[test]
    fn a_strip_narrower_than_the_pet_pins_it_to_the_start() {
        let mut pet = Pet::new(9, 0);
        let views = run(&mut pet, 0, 60_000, 30.0, 48.0);
        assert!(views.iter().all(|view| view.x == 0.0));
    }
}
