//! 候选窗上的角色:由一份行为定义驱动,在一块矩形区域里移动,可以有很多个。平台无关。
//!
//! 行为定义是数据,皮肤或扩展写出来即可,不需要代码(见 [`Behavior`]):每个状态引用动画
//! 片段的名字、可选的移动方式、停留时长;转移由触发条件与权重决定。触发条件有四种:
//! 时间到了([`Trigger::Timeout`])、有按键([`Trigger::Key`])、一段时间没按键
//! ([`Trigger::Quiet`])、碰到区域边缘([`Trigger::Edge`])。
//!
//! 按键与时间由宿主喂进来,这里不读时钟;随机数是固定种子的 xorshift,同一种子同样的轨迹。
//! 角色画在文字上面还是下面,不在这里决定,由扩展的设置决定。

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

/// 可以走的方向:只沿水平方向,或四面八方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axes {
    Horizontal,
    Free,
}

/// 行走参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Walk {
    /// 速度(像素/秒)。
    pub speed: f32,
    pub axes: Axes,
}

/// 转移的触发条件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// 当前状态的停留时长到了。
    Timeout,
    /// 有按键。
    Key,
    /// 距离上一次按键已经过了这么多毫秒。
    Quiet(u64),
    /// 行走时碰到区域的边缘。
    Edge,
}

/// 一个状态。
#[derive(Clone, Debug, PartialEq)]
pub struct StateDef {
    /// 用哪个动画片段(名字见 `animation::Pack`)。
    pub clip: String,
    /// 有行走参数时,进入状态后沿某个方向走。
    pub walk: Option<Walk>,
    /// 停留时长范围(毫秒,闭区间);`None` 表示不按时间离开。
    pub duration: Option<(u64, u64)>,
}

/// 一条转移:从 `from` 到 `to`,由 `trigger` 触发,同类转移之间按 `weight` 随机选。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub from: usize,
    pub to: usize,
    pub trigger: Trigger,
    pub weight: u32,
}

/// 行为定义:状态、转移与起始状态。
#[derive(Clone, Debug, PartialEq)]
pub struct Behavior {
    pub states: Vec<StateDef>,
    pub transitions: Vec<Transition>,
    pub start: usize,
}

impl Behavior {
    /// 检查定义是否自洽:下标都存在、权重为正、时长范围不倒置。
    pub fn validate(&self) -> Result<(), String> {
        let count = self.states.len();
        if count == 0 {
            return Err("行为没有任何状态".into());
        }
        if self.start >= count {
            return Err(format!("起始状态 {} 不存在", self.start));
        }
        for (index, state) in self.states.iter().enumerate() {
            if state.clip.is_empty() {
                return Err(format!("状态 {index} 没有动画片段"));
            }
            if let Some((lo, hi)) = state.duration
                && lo > hi
            {
                return Err(format!("状态 {index} 的停留时长范围倒置"));
            }
        }
        for transition in &self.transitions {
            if transition.from >= count || transition.to >= count {
                return Err(format!(
                    "转移 {}→{} 指向不存在的状态",
                    transition.from, transition.to
                ));
            }
            if transition.weight == 0 {
                return Err("转移权重必须大于 0".into());
            }
        }
        Ok(())
    }
}

/// 宿主需要的全部信息:当前状态的下标、精灵左上角的位置(像素)、朝向。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub state: usize,
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
    behavior: Behavior,
    state: usize,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    facing: Facing,
    /// 当前状态结束的时刻;`None` 表示不按时间离开。
    until_ms: Option<u64>,
    last_ms: u64,
    last_key_ms: u64,
    key_pending: bool,
    rng: Rng,
}

impl Actor {
    /// 创建一个角色,从行为定义的起始状态开始。`start` 是精灵左上角的初始位置。
    pub fn new(
        seed: u64,
        now_ms: u64,
        start: (f32, f32),
        behavior: Behavior,
    ) -> Result<Self, String> {
        behavior.validate()?;
        let state = behavior.start;
        let mut actor = Self {
            behavior,
            state,
            x: start.0,
            y: start.1,
            vx: 0.0,
            vy: 0.0,
            facing: Facing::Right,
            until_ms: None,
            last_ms: now_ms,
            last_key_ms: now_ms,
            key_pending: false,
            rng: Rng::new(seed),
        };
        actor.enter(state, now_ms);
        Ok(actor)
    }

    /// 有按键时调用。
    pub fn key(&mut self, now_ms: u64) {
        self.last_key_ms = now_ms;
        self.key_pending = true;
    }

    /// 当前状态用的动画片段名字。
    pub fn clip(&self) -> &str {
        &self.behavior.states[self.state].clip
    }

    /// 推进到 `now_ms`。`area` 是角色可以活动的区域,`sprite` 是精灵的宽高(像素)。
    /// 精灵整个都留在区域里;区域比精灵还小时,精灵贴着左上角。
    pub fn tick(&mut self, now_ms: u64, area: Rect, sprite: (f32, f32)) -> View {
        let bounds = Bounds::of(area, sprite);
        let dt = now_ms.saturating_sub(self.last_ms) as f32 / 1000.0;
        self.last_ms = now_ms;
        let quiet = now_ms.saturating_sub(self.last_key_ms);
        if std::mem::take(&mut self.key_pending) {
            self.pick(now_ms, |t| t.trigger == Trigger::Key);
        }
        self.pick(now_ms, |t| matches!(t.trigger, Trigger::Quiet(ms) if quiet >= ms));
        if self.until_ms.is_some_and(|until| now_ms >= until) {
            self.pick(now_ms, |t| t.trigger == Trigger::Timeout);
        }
        if self.behavior.states[self.state].walk.is_some() {
            self.step(now_ms, dt, bounds);
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

    /// 进入某个状态:定停留时长,按行走参数定速度。
    fn enter(&mut self, state: usize, now_ms: u64) {
        self.state = state;
        let duration = self.behavior.states[state].duration;
        let walk = self.behavior.states[state].walk;
        self.until_ms = duration.map(|(lo, hi)| now_ms + self.rng.range(lo, hi));
        match walk {
            Some(walk) => {
                let (dx, dy) = match walk.axes {
                    Axes::Horizontal => {
                        if self.rng.range(0, 1) == 0 {
                            (1.0, 0.0)
                        } else {
                            (-1.0, 0.0)
                        }
                    }
                    Axes::Free => DIRECTIONS[self.rng.range(0, 7) as usize],
                };
                self.vx = dx * walk.speed;
                self.vy = dy * walk.speed;
            }
            None => {
                self.vx = 0.0;
                self.vy = 0.0;
            }
        }
    }

    /// 从当前状态出发,按触发条件筛出候选转移,按权重随机挑一条并进入。挑中返回 `true`。
    fn pick(&mut self, now_ms: u64, matches: impl Fn(&Transition) -> bool) -> bool {
        let from = self.state;
        let candidates: Vec<(usize, u32)> = self
            .behavior
            .transitions
            .iter()
            .filter(|&t| t.from == from && matches(t))
            .map(|t| (t.to, t.weight))
            .collect();
        let total: u64 = candidates.iter().map(|&(_, w)| u64::from(w)).sum();
        if total == 0 {
            return false;
        }
        let mut roll = self.rng.range(0, total - 1);
        for (to, weight) in candidates {
            let weight = u64::from(weight);
            if roll < weight {
                self.enter(to, now_ms);
                return true;
            }
            roll -= weight;
        }
        false
    }

    /// 走一小步。碰到区域边缘时,如果当前状态有边缘转移就换状态(新方向重新选);
    /// 否则停在边上反弹。
    fn step(&mut self, now_ms: u64, dt: f32, bounds: Bounds) {
        let nx = self.x + self.vx * dt;
        let ny = self.y + self.vy * dt;
        let hit = nx < bounds.min_x || nx > bounds.max_x || ny < bounds.min_y || ny > bounds.max_y;
        if hit && self.pick(now_ms, |t| t.trigger == Trigger::Edge) {
            self.x = nx.clamp(bounds.min_x, bounds.max_x);
            self.y = ny.clamp(bounds.min_y, bounds.max_y);
            return;
        }
        let (x, vx) = bounce(nx, self.vx, bounds.min_x, bounds.max_x);
        let (y, vy) = bounce(ny, self.vy, bounds.min_y, bounds.max_y);
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

/// 预设:横带里的小猫。待机、走、玩;看着输入;一分钟没输入就睡,按键即醒。
pub fn strip_cat() -> Behavior {
    let state = |clip: &str, walk: Option<Walk>, duration: Option<(u64, u64)>| StateDef {
        clip: clip.to_string(),
        walk,
        duration,
    };
    let tr = |from, to, trigger, weight| Transition {
        from,
        to,
        trigger,
        weight,
    };
    Behavior {
        states: vec![
            state("idle", None, Some((1_000, 4_000))),
            state(
                "walk",
                Some(Walk {
                    speed: 28.0,
                    axes: Axes::Horizontal,
                }),
                Some((2_000, 6_000)),
            ),
            state("play", None, Some((1_500, 3_000))),
            state("typing", None, None),
            state("sleep", None, None),
        ],
        transitions: vec![
            tr(0, 1, Trigger::Timeout, 3),
            tr(0, 2, Trigger::Timeout, 2),
            tr(1, 0, Trigger::Timeout, 2),
            tr(1, 2, Trigger::Timeout, 1),
            tr(2, 0, Trigger::Timeout, 1),
            tr(2, 1, Trigger::Timeout, 1),
            tr(0, 3, Trigger::Key, 1),
            tr(1, 3, Trigger::Key, 1),
            tr(2, 3, Trigger::Key, 1),
            tr(4, 3, Trigger::Key, 1),
            tr(3, 0, Trigger::Quiet(1_200), 1),
            tr(0, 4, Trigger::Quiet(60_000), 1),
            tr(1, 4, Trigger::Quiet(60_000), 1),
            tr(2, 4, Trigger::Quiet(60_000), 1),
        ],
        start: 0,
    }
}

/// 预设:候选窗里到处爬的小蜘蛛。不怕打字,不睡;碰到边缘就换个方向。
pub fn spider() -> Behavior {
    let state = |clip: &str, walk: Option<Walk>, duration: Option<(u64, u64)>| StateDef {
        clip: clip.to_string(),
        walk,
        duration,
    };
    let tr = |from, to, trigger, weight| Transition {
        from,
        to,
        trigger,
        weight,
    };
    Behavior {
        states: vec![
            state("idle", None, Some((300, 1_500))),
            state(
                "walk",
                Some(Walk {
                    speed: 40.0,
                    axes: Axes::Free,
                }),
                Some((1_000, 3_000)),
            ),
            state("wiggle", None, Some((300, 800))),
        ],
        transitions: vec![
            tr(0, 1, Trigger::Timeout, 3),
            tr(0, 2, Trigger::Timeout, 1),
            tr(1, 0, Trigger::Timeout, 2),
            tr(1, 2, Trigger::Timeout, 1),
            tr(1, 1, Trigger::Edge, 1),
            tr(2, 0, Trigger::Timeout, 1),
            tr(2, 1, Trigger::Timeout, 1),
        ],
        start: 0,
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
        Self {
            min_x: area.left,
            max_x: (area.right - sprite.0).max(area.left),
            min_y: area.top,
            max_y: (area.bottom - sprite.1).max(area.top),
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

    fn spawn(seed: u64, start: (f32, f32), behavior: Behavior) -> Actor {
        Actor::new(seed, 0, start, behavior).expect("预设行为合法")
    }

    #[test]
    fn presets_are_valid_behaviors() {
        assert!(strip_cat().validate().is_ok());
        assert!(spider().validate().is_ok());
    }

    #[test]
    fn invalid_behaviors_are_rejected() {
        let mut bad = strip_cat();
        bad.transitions.push(Transition {
            from: 0,
            to: 9,
            trigger: Trigger::Timeout,
            weight: 1,
        });
        assert!(bad.validate().unwrap_err().contains("指向不存在"));
        let mut zero = strip_cat();
        zero.transitions[0].weight = 0;
        assert!(zero.validate().unwrap_err().contains("权重"));
        let mut backwards = spider();
        backwards.states[0].duration = Some((5, 1));
        assert!(backwards.validate().unwrap_err().contains("倒置"));
    }

    #[test]
    fn free_actor_stays_inside_the_whole_region() {
        let sprite = (48.0, 48.0);
        let mut actor = spawn(7, (10.0, 10.0), spider());
        let views = run(&mut actor, 0, 600_000, sprite);
        assert!(views.iter().all(|view| {
            (0.0..=300.0 - sprite.0).contains(&view.x) && (0.0..=200.0 - sprite.1).contains(&view.y)
        }));
        assert!(views.iter().any(|view| view.y > 100.0));
        assert!(views.iter().any(|view| view.x > 150.0));
    }

    #[test]
    fn horizontal_walkers_keep_their_row_and_turn_at_the_ends() {
        let sprite = (48.0, 48.0);
        let max_x = 300.0 - sprite.0;
        let mut actor = spawn(7, (0.0, 60.0), strip_cat());
        let views = run(&mut actor, 0, 600_000, sprite);
        assert!(views.iter().all(|view| view.y == 60.0));
        let walk = strip_cat();
        for view in &views {
            if walk.states[view.state].clip == "walk" && view.x >= max_x - 0.001 {
                assert_eq!(view.facing, Facing::Left);
            }
            if walk.states[view.state].clip == "walk" && view.x <= 0.001 {
                assert_eq!(view.facing, Facing::Right);
            }
        }
    }

    #[test]
    fn watching_actor_stops_while_the_user_types() {
        let mut actor = spawn(3, (100.0, 100.0), strip_cat());
        actor.key(0);
        let views = run(&mut actor, 0, 1_150, (48.0, 48.0));
        assert!(views.iter().all(|view| actor.behavior.states[view.state].clip == "typing"));
        assert!(views.windows(2).all(|pair| pair[0].x == pair[1].x && pair[0].y == pair[1].y));
    }

    #[test]
    fn only_sleepers_fall_asleep_and_wake_on_a_key() {
        let mut cat = spawn(3, (0.0, 60.0), strip_cat());
        cat.key(0);
        run(&mut cat, 0, 61_000, (48.0, 48.0));
        let asleep = run(&mut cat, 61_000, 30_000, (48.0, 48.0));
        assert!(asleep.iter().all(|view| cat.behavior.states[view.state].clip == "sleep"));
        cat.key(91_000);
        let woke = cat.tick(91_050, area(), (48.0, 48.0));
        assert_eq!(cat.behavior.states[woke.state].clip, "typing");

        let mut spider = spawn(3, (0.0, 0.0), spider());
        let never = run(&mut spider, 0, 300_000, (48.0, 48.0));
        assert!(never.iter().all(|view| spider.behavior.states[view.state].clip != "sleep"));
    }

    #[test]
    fn a_crowd_fills_the_region_with_different_paths() {
        let sprite = (16.0, 16.0);
        let mut crowd: Vec<Actor> = (0..20)
            .map(|seed| spawn(seed + 1, (150.0, 100.0), spider()))
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
        let mut first = spawn(42, (0.0, 0.0), spider());
        let mut second = spawn(42, (0.0, 0.0), spider());
        assert_eq!(
            run(&mut first, 0, 120_000, (48.0, 48.0)),
            run(&mut second, 0, 120_000, (48.0, 48.0))
        );
    }

    #[test]
    fn a_region_smaller_than_the_sprite_pins_it_to_the_corner() {
        let mut actor = spawn(9, (50.0, 50.0), spider());
        let tiny = Rect::from_xywh(10.0, 10.0, 30.0, 30.0);
        for now in (0..60_000).step_by(50) {
            let view = actor.tick(now, tiny, (48.0, 48.0));
            assert_eq!((view.x, view.y), (10.0, 10.0));
        }
    }
}
