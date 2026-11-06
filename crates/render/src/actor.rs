//! 候选窗上的角色:在一块矩形区域里移动,可以有很多个。平台无关。
//!
//! 角色只回答三件事:现在是什么状态、在哪里、朝哪边。画面由宿主按 [`View`] 取动画帧
//! (见 `animation::frame_at`)。按键与时间由宿主喂进来,这里不读时钟;随机数是固定种子的
//! xorshift,同一种子同样的轨迹,便于测试。
//!
//! 一只沿横带走的小猫、一群在整个候选窗里爬的小蜘蛛,是同一种角色,只是 [`Motion`] 与
//! [`Habits`] 参数不同。下面的 [`STRIP_CAT`] 与 [`SPIDER`] 只是两个预设。

use crate::animation::{SLEEP_MS, State, TYPING_MS};
use crate::panel::Rect;

/// 八个方向的单位向量(近似),用来随机选行走方向,不需要三角函数。
const DIRECTIONS: [(f32, f32); 8] = [
    (1.0, 0.0),
    (0.707, 0.707),
    (0.0, 1.0),
    (-0.707, 0.707),
    (-1.0, 0.0),
    (-0.707, -0.707),
    (0.0, -1.0),
    (0.707, -0.707),
];

/// 朝向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facing {
    Left,
    Right,
}

/// 可以走的方向:只沿水平方向(如横带里的猫),或四面八方(如蜘蛛)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axes {
    Horizontal,
    Free,
}

/// 移动参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    /// 行走速度(像素/秒)。
    pub speed: f32,
    pub axes: Axes,
}

/// 习性:会不会躲着键盘、会不会睡,以及各种行为的持续时间范围(毫秒)。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Habits {
    /// 打字时停下来看着输入。
    pub watches_typing: bool,
    /// 长时间没有输入时睡下,按键即醒。
    pub sleeps: bool,
    pub walk_ms: (u64, u64),
    pub play_ms: (u64, u64),
    pub idle_ms: (u64, u64),
}

/// 预设:横带里的小猫,看着输入、会睡。
pub const STRIP_CAT: (Motion, Habits) = (
    Motion {
        speed: 28.0,
        axes: Axes::Horizontal,
    },
    Habits {
        watches_typing: true,
        sleeps: true,
        walk_ms: (2_000, 6_000),
        play_ms: (1_500, 3_000),
        idle_ms: (1_000, 4_000),
    },
);

/// 预设:候选窗里到处爬的小蜘蛛,不怕打字,不睡。
pub const SPIDER: (Motion, Habits) = (
    Motion {
        speed: 40.0,
        axes: Axes::Free,
    },
    Habits {
        watches_typing: false,
        sleeps: false,
        walk_ms: (1_000, 3_000),
        play_ms: (300, 800),
        idle_ms: (300, 1_500),
    },
);

/// 宿主需要的全部信息:状态、精灵左上角的位置(像素)、朝向。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub state: State,
    pub x: f32,
    pub y: f32,
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

/// 一个角色。
#[derive(Clone, Debug)]
pub struct Actor {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    facing: Facing,
    state: State,
    /// 当前这段行为结束的时刻。
    until_ms: u64,
    last_ms: u64,
    last_key_ms: u64,
    motion: Motion,
    habits: Habits,
    rng: Rng,
}

impl Actor {
    /// 创建时视为刚刚有过输入,从待机开始;`start` 是精灵左上角的初始位置。
    pub fn new(seed: u64, now_ms: u64, start: (f32, f32), preset: (Motion, Habits)) -> Self {
        let (motion, habits) = preset;
        Self {
            x: start.0,
            y: start.1,
            vx: 0.0,
            vy: 0.0,
            facing: Facing::Right,
            state: State::Idle,
            until_ms: now_ms,
            last_ms: now_ms,
            last_key_ms: now_ms,
            motion,
            habits,
            rng: Rng::new(seed),
        }
    }

    /// 有按键时调用。
    pub fn key(&mut self, now_ms: u64) {
        self.last_key_ms = now_ms;
    }

    /// 推进到 `now_ms`。`area` 是角色可以活动的区域,`sprite` 是精灵的宽高(像素)。
    /// 精灵整个都留在区域里;区域比精灵还小时,精灵贴着左上角。
    pub fn tick(&mut self, now_ms: u64, area: Rect, sprite: (f32, f32)) -> View {
        let bounds = Bounds::of(area, sprite);
        let dt = now_ms.saturating_sub(self.last_ms) as f32 / 1000.0;
        self.last_ms = now_ms;
        let quiet = now_ms.saturating_sub(self.last_key_ms);
        if self.habits.watches_typing && quiet < TYPING_MS {
            self.rest(State::Typing);
        } else if self.habits.sleeps && quiet >= SLEEP_MS {
            self.rest(State::Sleep);
        } else if matches!(self.state, State::Typing | State::Sleep) || now_ms >= self.until_ms {
            self.choose(now_ms);
        }
        if self.state == State::Walk {
            self.step(dt, bounds);
        }
        self.x = self.x.clamp(bounds.min_x, bounds.max_x);
        self.y = self.y.clamp(bounds.min_y, bounds.max_y);
        View {
            state: self.state,
            x: self.x,
            y: self.y,
            facing: self.facing,
        }
    }

    fn rest(&mut self, state: State) {
        self.state = state;
        self.vx = 0.0;
        self.vy = 0.0;
    }

    /// 从一段空闲里挑下一段行为:走、玩或待机。
    fn choose(&mut self, now_ms: u64) {
        let roll = self.rng.range(0, 99);
        let (state, span) = if roll < 60 {
            (State::Walk, self.habits.walk_ms)
        } else if roll < 80 {
            (State::Play, self.habits.play_ms)
        } else {
            (State::Idle, self.habits.idle_ms)
        };
        self.state = state;
        self.until_ms = now_ms + self.rng.range(span.0, span.1);
        if state == State::Walk {
            let (dx, dy) = match self.motion.axes {
                Axes::Horizontal => {
                    if self.rng.range(0, 1) == 0 {
                        (1.0, 0.0)
                    } else {
                        (-1.0, 0.0)
                    }
                }
                Axes::Free => DIRECTIONS[self.rng.range(0, 7) as usize],
            };
            self.vx = dx * self.motion.speed;
            self.vy = dy * self.motion.speed;
        } else {
            self.vx = 0.0;
            self.vy = 0.0;
        }
    }

    /// 走一小步;碰到区域的边就停在边上并反弹。
    fn step(&mut self, dt: f32, bounds: Bounds) {
        let (x, vx) = bounce(self.x + self.vx * dt, self.vx, bounds.min_x, bounds.max_x);
        let (y, vy) = bounce(self.y + self.vy * dt, self.vy, bounds.min_y, bounds.max_y);
        self.x = x;
        self.vx = vx;
        self.y = y;
        self.vy = vy;
        if self.vx < 0.0 {
            self.facing = Facing::Left;
        } else if self.vx > 0.0 {
            self.facing = Facing::Right;
        }
    }
}

/// 精灵左上角可以到达的范围。
#[derive(Clone, Copy, Debug)]
struct Bounds {
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
}

impl Bounds {
    fn of(area: Rect, sprite: (f32, f32)) -> Self {
        let max_x = (area.right - sprite.0).max(area.left);
        let max_y = (area.bottom - sprite.1).max(area.top);
        Self {
            min_x: area.left,
            max_x,
            min_y: area.top,
            max_y,
        }
    }
}

/// 一维的碰壁反弹:位置越界就停在边上,速度换向。
fn bounce(next: f32, velocity: f32, min: f32, max: f32) -> (f32, f32) {
    if next < min {
        (min, velocity.abs())
    } else if next > max {
        (max, -velocity.abs())
    } else {
        (next, velocity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_xywh(0.0, 0.0, 300.0, 200.0)
    }

    /// 每 50 毫秒推进一次,跑 `total_ms` 毫秒,返回每一步的视图。
    fn run(actor: &mut Actor, from_ms: u64, total_ms: u64, sprite: (f32, f32)) -> Vec<View> {
        (from_ms..from_ms + total_ms)
            .step_by(50)
            .map(|now| actor.tick(now, area(), sprite))
            .collect()
    }

    #[test]
    fn free_actor_stays_inside_the_whole_region() {
        let sprite = (48.0, 48.0);
        let mut actor = Actor::new(7, 0, (10.0, 10.0), SPIDER);
        let views = run(&mut actor, 0, 600_000, sprite);
        assert!(views.iter().all(|view| {
            (0.0..=300.0 - sprite.0).contains(&view.x) && (0.0..=200.0 - sprite.1).contains(&view.y)
        }));
        assert!(views.iter().any(|view| view.y > 100.0));
        assert!(views.iter().any(|view| view.x > 150.0));
    }

    #[test]
    fn horizontal_actor_never_leaves_its_row_and_turns_at_the_ends() {
        let sprite = (48.0, 48.0);
        let max_x = 300.0 - sprite.0;
        let mut actor = Actor::new(7, 0, (0.0, 60.0), STRIP_CAT);
        let views = run(&mut actor, 0, 600_000, sprite);
        assert!(views.iter().all(|view| view.y == 60.0));
        for view in &views {
            if view.state == State::Walk && view.x >= max_x - 0.001 {
                assert_eq!(view.facing, Facing::Left);
            }
            if view.state == State::Walk && view.x <= 0.001 {
                assert_eq!(view.facing, Facing::Right);
            }
        }
    }

    #[test]
    fn watching_actor_stops_while_the_user_types() {
        let mut actor = Actor::new(3, 0, (100.0, 100.0), STRIP_CAT);
        actor.key(0);
        let views = run(&mut actor, 0, TYPING_MS - 50, (48.0, 48.0));
        assert!(views.iter().all(|view| view.state == State::Typing));
        assert!(
            views
                .windows(2)
                .all(|pair| pair[0].x == pair[1].x && pair[0].y == pair[1].y)
        );
    }

    #[test]
    fn only_sleepers_fall_asleep_and_wake_on_a_key() {
        let mut cat = Actor::new(3, 0, (0.0, 60.0), STRIP_CAT);
        cat.key(0);
        let asleep = run(&mut cat, SLEEP_MS, 30_000, (48.0, 48.0));
        assert!(asleep.iter().all(|view| view.state == State::Sleep));
        cat.key(SLEEP_MS + 30_000);
        assert_eq!(
            cat.tick(SLEEP_MS + 30_050, area(), (48.0, 48.0)).state,
            State::Typing
        );

        let mut spider = Actor::new(3, 0, (0.0, 0.0), SPIDER);
        let never = run(&mut spider, 0, 300_000, (48.0, 48.0));
        assert!(never.iter().all(|view| view.state != State::Sleep));
    }

    #[test]
    fn a_crowd_fills_the_region_with_different_paths() {
        let sprite = (16.0, 16.0);
        let mut crowd: Vec<Actor> = (0..20)
            .map(|seed| Actor::new(seed + 1, 0, (150.0, 100.0), SPIDER))
            .collect();
        let mut seen = std::collections::HashSet::new();
        for now in (0..120_000).step_by(50) {
            for actor in &mut crowd {
                let view = actor.tick(now, area(), sprite);
                assert!((0.0..=300.0 - sprite.0).contains(&view.x));
                assert!((0.0..=200.0 - sprite.1).contains(&view.y));
                seen.insert((view.x as i32 / 10, view.y as i32 / 10));
            }
        }
        assert!(seen.len() > 100, "只覆盖了 {} 格", seen.len());
    }

    #[test]
    fn same_seed_gives_the_same_path() {
        let mut first = Actor::new(42, 0, (0.0, 0.0), SPIDER);
        let mut second = Actor::new(42, 0, (0.0, 0.0), SPIDER);
        assert_eq!(
            run(&mut first, 0, 120_000, (48.0, 48.0)),
            run(&mut second, 0, 120_000, (48.0, 48.0))
        );
    }

    #[test]
    fn a_region_smaller_than_the_sprite_pins_it_to_the_corner() {
        let mut actor = Actor::new(9, 0, (50.0, 50.0), SPIDER);
        let tiny = Rect::from_xywh(10.0, 10.0, 30.0, 30.0);
        for now in (0..60_000).step_by(50) {
            let view = actor.tick(now, tiny, (48.0, 48.0));
            assert_eq!((view.x, view.y), (10.0, 10.0));
        }
    }
}
