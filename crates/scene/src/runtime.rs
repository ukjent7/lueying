//! 场景运行时:确定性时间线,吃信号与事件,出一帧绘制清单。
//!
//! 运行时不碰时钟、不碰文字、不碰 GPU:时间由调用方按帧喂入,输出是
//! 纯数据(锚矩形 + 图元)。所有随机都走 [`expr::seeded`],同输入必同
//! 输出,Linux CI 上的黄金测试才立得住。表达式预算超限只在帧上打
//! `degraded` 标记,由窗口层决定熔断;整层永不 panic。

use std::collections::HashMap;

use crate::expr::{self, Expr};
use crate::model::{
    Action, Anchor, AnchorBase, Edge, Fill, Layer, Node, NodeBody, Rule, Scene, Shape, TweenProp,
};

/// 每帧表达式操作预算;所有节点与规则共享,超限整帧降级。
const EXPR_OPS: u32 = 4096;
/// 单个发射器的粒子池上限;满了就跳过生成。
const MAX_PARTICLES: usize = 256;
/// 一帧最多处理的事件数(含 emit 出来的自定义事件),防规则互相点燃。
const MAX_EVENTS: usize = 64;
/// 粒子按固定步长模拟,一段 dt 最多走这么多步,防大 dt 爆算。
const MAX_STEPS: u32 = 8;
const PARTICLE_DT: f64 = 1.0 / 120.0;

/// 锚矩形:[x, y, 宽, 高],逻辑像素。
pub type Rect = [f32; 4];

/// 一帧的输入信号;全部由窗口层供给,运行时不自己测量。
pub struct Inputs<'a> {
    pub typing_rate: f64,
    pub candidates: usize,
    pub page: usize,
    pub page_count: usize,
    pub mode_chinese: bool,
    pub selected: usize,
    /// 距上次击键的秒数。
    pub idle: f64,
    pub panel: Rect,
    pub preedit: Rect,
    pub highlight: Option<Rect>,
    pub candidates: &'a [Rect],
    pub caret: Rect,
    pub screen: Rect,
}

/// 事件名;内置事件由窗口层发,自定义事件由规则 emit。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventName {
    Key,
    Commit,
    Select,
    Page,
    BackspaceBurst,
    Correction,
    ModeSwitch,
    Custom(String),
}

impl EventName {
    fn matches(&self, on: &str) -> bool {
        match self {
            Self::Key => on == "key",
            Self::Commit => on == "commit",
            Self::Select => on == "select",
            Self::Page => on == "page",
            Self::BackspaceBurst => on == "backspace_burst",
            Self::Correction => on == "correction",
            Self::ModeSwitch => on == "mode_switch",
            Self::Custom(name) => name == on,
        }
    }
}

/// 一帧绘制清单;`items` 已按 below-text 在前排好序。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    pub items: Vec<Item>,
    /// 表达式预算超限;窗口层据此熔断场景。
    pub degraded: bool,
}

/// 一个待绘制图元。
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub layer: Layer,
    /// 相对窗口的逻辑像素矩形。
    pub rect: Rect,
    pub opacity: f32,
    pub effect: Option<EffectOut>,
    pub body: Body,
}

/// 特效求值结果;窗口层接成 D2D 内置特效图。
#[derive(Debug, Clone, PartialEq)]
pub enum EffectOut {
    Blur {
        radius: f32,
    },
    Shadow {
        color: Option<Fill>,
        offset: [f32; 2],
    },
}

/// 图元本体。
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Shape {
        shape: Shape,
        radius: f32,
        stroke: f32,
        fill: Fill,
    },
    Image {
        src: String,
    },
    Sprite {
        src: String,
        frame: u32,
        grid: [u32; 2],
    },
    Text {
        content: String,
        font_size: f32,
        fill: Fill,
    },
    /// 粒子坐标相对 `rect` 左上角;形状与颜色随节点。
    Particles {
        shape: Shape,
        fill: Fill,
        particles: Vec<Particle>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Particle {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub opacity: f32,
}

struct Tween {
    node: String,
    prop: TweenProp,
    from: f32,
    to: f32,
    start: f64,
    over: f64,
    ease: Option<usize>,
}

struct SpriteState {
    clip: String,
    time: f64,
}

struct Live {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    age: f64,
    life: f64,
    size: f32,
}

#[derive(Default)]
struct EmitterState {
    acc: f64,
    rng: u64,
    live: Vec<Live>,
}

/// 运行期可变状态;与场景定义分离,黄金测试可以快照对比。
#[derive(Default)]
struct State {
    vars: Vec<f64>,
    tweens: Vec<Tween>,
    delays: Vec<(f64, Vec<Action>)>,
    sprites: HashMap<String, SpriteState>,
    springs: HashMap<String, [f32; 2]>,
    emitters: HashMap<String, EmitterState>,
}

/// 求值与装配阶段用的只读快照;字段从 State 里拆出来,借用互不打架。
struct View<'a> {
    vars: &'a [f64],
    tweens: &'a [Tween],
    sprites: &'a HashMap<String, SpriteState>,
    springs: &'a HashMap<String, [f32; 2]>,
    emitters: &'a HashMap<String, EmitterState>,
}

/// 场景运行时;一个场景一份,窗口层长期持有。
pub struct Runtime {
    scene: Scene,
    state: State,
}

impl Runtime {
    pub fn new(scene: Scene) -> Self {
        Self {
            state: State {
                vars: vec![0.0; scene.vars.len()],
                ..State::default()
            },
            scene,
        }
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// 推进一个时间片;`dt` 单位秒。同一帧内事件按序触发,emit 出来的
    /// 自定义事件接在同一条队列后面,超过 [`MAX_EVENTS`] 直接弃掉。
    pub fn tick(&mut self, time: f64, dt: f64, inputs: &Inputs<'_>, events: &[EventName]) -> Frame {
        let mut budget = EXPR_OPS;
        let mut frame = Frame::default();
        let scene = &self.scene;

        // 首次见到的发射器与跟随精灵先注册状态;随机源按场景与节点名定种。
        for node in &scene.nodes {
            match &node.body {
                NodeBody::Emitter { .. } => {
                    self.state
                        .emitters
                        .entry(node.id.clone())
                        .or_insert_with(|| EmitterState {
                            rng: expr::seeded(&mut (scene.seed ^ fnv(node.id.as_bytes()))),
                            ..EmitterState::default()
                        });
                }
                NodeBody::Sprite {
                    follow: Some(_), ..
                } => {
                    self.state
                        .springs
                        .entry(node.id.clone())
                        .or_insert_with(|| {
                            anchor_rect(&node.anchor, inputs).map_or([0.0, 0.0], |rect| {
                                let (cx, cy) = edge_point(rect, node.anchor.edge);
                                [cx, cy]
                            })
                        });
                }
                _ => {}
            }
        }

        let State {
            vars,
            tweens,
            delays,
            sprites,
            springs,
            emitters,
        } = &mut self.state;

        // 到期的延时动作先于本帧事件执行。
        let mut index = 0;
        while index < delays.len() {
            if delays[index].0 <= time {
                let (_, actions) = delays.remove(index);
                run_actions(
                    scene,
                    vars,
                    tweens,
                    sprites,
                    delays,
                    inputs,
                    time,
                    &actions,
                    &mut Vec::new(),
                    &mut budget,
                    &mut frame.degraded,
                    0,
                );
            } else {
                index += 1;
            }
        }

        // 变量积分在事件之前:规则 set 写下的值这一帧保持原样,
        // 下一帧才开始被 from 拉走或衰减。
        for (index, decl) in scene.vars.iter().enumerate() {
            if let Some(from) = &decl.from {
                let target = eval(from, scene, vars, inputs, &mut budget, &mut frame.degraded);
                let value = &mut vars[index];
                if target > *value {
                    // 上升:每秒逼近剩余差距的比例 gain。
                    *value += (target - *value) * (decl.gain * dt).min(1.0);
                } else {
                    // 衰减朝 0,时间常数 decay 秒。
                    *value *= (-dt / decl.decay).exp();
                }
            }
        }

        // 事件按序触发规则。
        let mut queue: Vec<EventName> = events.to_vec();
        let mut index = 0;
        while index < queue.len() && index < MAX_EVENTS {
            let event = queue[index].clone();
            index += 1;
            for rule_index in 0..scene.rules.len() {
                let Rule { on, when, actions } = &scene.rules[rule_index];
                if !event.matches(on) {
                    continue;
                }
                let pass = when.map_or(true, |expr| {
                    eval(expr, scene, vars, inputs, &mut budget, &mut frame.degraded) != 0.0
                });
                if pass {
                    run_actions(
                        scene,
                        vars,
                        tweens,
                        sprites,
                        delays,
                        inputs,
                        time,
                        actions,
                        &mut queue,
                        &mut budget,
                        &mut frame.degraded,
                        0,
                    );
                }
            }
        }

        // 精灵帧时钟前进;播放状态与补间都按 time 求,无需显式推进。
        for sprite in sprites.values_mut() {
            sprite.time += dt;
        }

        // 粒子模拟:生成 + 固定步长积分。
        for (id, emitter) in emitters.iter_mut() {
            let Some(node) = scene.nodes.iter().find(|node| &node.id == id) else {
                continue;
            };
            let NodeBody::Emitter {
                rate,
                life,
                speed,
                direction,
                spread,
                gravity,
                size,
                ..
            } = &node.body
            else {
                continue;
            };
            let rate =
                eval(rate, scene, vars, inputs, &mut budget, &mut frame.degraded).clamp(0.0, 500.0);
            // 出生点取锚边点,坐标相对锚矩形左上角,锚动粒子跟着动。
            let origin = anchor_rect(&node.anchor, inputs).map_or([0.0_f32, 0.0], |rect| {
                let (cx, cy) = edge_point(rect, node.anchor.edge);
                [cx - rect[0], cy - rect[1]]
            });
            emitter.acc += rate * dt;
            while emitter.acc >= 1.0 {
                emitter.acc -= 1.0;
                if emitter.live.len() >= MAX_PARTICLES {
                    break;
                }
                let angle =
                    (direction + (seeded_f32(&mut emitter.rng) - 0.5) * 2.0 * spread).to_radians();
                let speed = speed[0] + (speed[1] - speed[0]) * seeded_f32(&mut emitter.rng);
                let size = size[0] + (size[1] - size[0]) * seeded_f32(&mut emitter.rng);
                emitter.live.push(Live {
                    x: origin[0],
                    y: origin[1],
                    vx: angle.cos() * speed,
                    vy: angle.sin() * speed,
                    age: 0.0,
                    life: *life,
                    size,
                });
            }
            let mut steps = 0;
            let mut left = dt;
            while left > 0.0 && steps < MAX_STEPS {
                let step = left.min(PARTICLE_DT);
                for particle in &mut emitter.live {
                    particle.age += step;
                    particle.x += particle.vx * step as f32;
                    particle.y += particle.vy * step as f32;
                    particle.vy += *gravity as f32 * step as f32;
                }
                left -= step;
                steps += 1;
            }
            emitter.live.retain(|particle| particle.age < *life);
        }

        // 弹簧跟随:精灵位置朝锚点指数逼近。
        for (id, position) in springs.iter_mut() {
            let Some(node) = scene.nodes.iter().find(|node| &node.id == id) else {
                continue;
            };
            let NodeBody::Sprite {
                follow: Some(follow),
                ..
            } = &node.body
            else {
                continue;
            };
            let Some(target) = anchor_rect(&node.anchor, inputs) else {
                continue;
            };
            let alpha = 1.0 - (-dt * f64::from(follow.spring)).exp();
            let (cx, cy) = edge_point(target, node.anchor.edge);
            position[0] += (cx - position[0]) * alpha as f32;
            position[1] += (cy - position[1]) * alpha as f32;
        }

        // 装配帧:below-text 在前,组内按文件顺序,稳定排序保序。
        let view = View {
            vars,
            tweens,
            sprites,
            springs,
            emitters,
        };
        let mut items = Vec::new();
        emit_nodes(
            scene,
            &view,
            inputs,
            time,
            &scene.nodes,
            1.0,
            &mut budget,
            &mut frame.degraded,
            &mut items,
        );
        items.sort_by_key(|item| item.layer == Layer::AboveText);
        frame.items = items;
        frame
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn seeded_f32(rng: &mut u64) -> f32 {
    expr::seeded(rng) as f32
}

fn builtin(name: &str, inputs: &Inputs<'_>) -> Option<f64> {
    Some(match name {
        "typing_rate" => inputs.typing_rate,
        "candidates" => inputs.candidates as f64,
        "page" => inputs.page as f64,
        "page_count" => inputs.page_count as f64,
        "mode" => f64::from(inputs.mode_chinese),
        "selected" => inputs.selected as f64,
        "idle" => inputs.idle,
        _ => return None,
    })
}

/// 表达式求值;预算超限打降级标并按 0 处理,帧不断。
fn eval(
    expr: &Expr,
    scene: &Scene,
    vars: &[f64],
    inputs: &Inputs<'_>,
    budget: &mut u32,
    degraded: &mut bool,
) -> f64 {
    let decls = &scene.vars;
    let names = |name: &str| -> Option<f64> {
        builtin(name, inputs).or_else(|| {
            decls
                .iter()
                .position(|var| var.name == name)
                .map(|index| vars[index])
        })
    };
    match expr.eval(&names, scene.seed, budget) {
        Ok(value) => value,
        Err(_) => {
            *degraded = true;
            0.0
        }
    }
}

/// 规则动作;`queue` 接住 emit 出来的事件,由外层事件循环继续处理。
#[allow(clippy::too_many_arguments)]
fn run_actions(
    scene: &Scene,
    vars: &mut Vec<f64>,
    tweens: &mut Vec<Tween>,
    sprites: &mut HashMap<String, SpriteState>,
    delays: &mut Vec<(f64, Vec<Action>)>,
    inputs: &Inputs<'_>,
    time: f64,
    actions: &[Action],
    queue: &mut Vec<EventName>,
    budget: &mut u32,
    degraded: &mut bool,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    for action in actions {
        match action {
            Action::Set { name, to } => {
                let value = eval(to, scene, vars, inputs, budget, degraded);
                if let Some(index) = scene.vars.iter().position(|var| &var.name == name) {
                    vars[index] = value;
                }
            }
            Action::Tween {
                node,
                prop,
                to,
                over,
                ease,
            } => {
                let to = eval(to, scene, vars, inputs, budget, degraded) as f32;
                let from = prop_current(
                    scene, tweens, vars, inputs, node, *prop, time, budget, degraded,
                );
                tweens.retain(|tween| tween.node != *node || tween.prop != *prop);
                tweens.push(Tween {
                    node: node.clone(),
                    prop: *prop,
                    from,
                    to,
                    start: time,
                    over: *over,
                    ease: *ease,
                });
            }
            Action::Play { node, clip } => {
                let entry = sprites.entry(node.clone()).or_insert_with(|| SpriteState {
                    clip: String::new(),
                    time: 0.0,
                });
                entry.clip = clip.clone();
                entry.time = 0.0;
            }
            Action::Emit { event } => {
                queue.push(EventName::Custom(event.clone()));
            }
            Action::Delay { after, then } => {
                delays.push((time + after, then.clone()));
            }
        }
    }
}

/// 属性当前值:最新补间优先,否则表达式的现值。
#[allow(clippy::too_many_arguments)]
fn prop_current(
    scene: &Scene,
    tweens: &[Tween],
    vars: &[f64],
    inputs: &Inputs<'_>,
    node_id: &str,
    prop: TweenProp,
    time: f64,
    budget: &mut u32,
    degraded: &mut bool,
) -> f32 {
    if let Some(tween) = tweens
        .iter()
        .rev()
        .find(|tween| tween.node == node_id && tween.prop == prop)
    {
        return tween_value(tween, time);
    }
    let Some(node) = scene.nodes.iter().find(|node| node.id == node_id) else {
        return 1.0;
    };
    match prop {
        TweenProp::Opacity => node
            .opacity
            .as_ref()
            .map_or(1.0, |expr| {
                eval(expr, scene, vars, inputs, budget, degraded) as f32
            })
            .clamp(0.0, 1.0),
        TweenProp::OffsetX => node.offset[0],
        TweenProp::OffsetY => node.offset[1],
    }
}

/// 锚解析;基准面缺失(没有选中项)返回 None,节点整颗跳过。
fn anchor_rect(anchor: &Anchor, inputs: &Inputs<'_>) -> Option<Rect> {
    Some(match anchor.base {
        AnchorBase::Panel => inputs.panel,
        AnchorBase::Preedit => inputs.preedit,
        AnchorBase::Highlight => inputs.highlight?,
        AnchorBase::Candidate(index) => *inputs.candidates.get(index)?,
        AnchorBase::Caret => inputs.caret,
        AnchorBase::Screen => inputs.screen,
    })
}

/// 锚边上的定位点。
fn edge_point(rect: Rect, edge: Edge) -> (f32, f32) {
    let [x, y, w, h] = rect;
    match edge {
        Edge::Center => (x + w * 0.5, y + h * 0.5),
        Edge::Top => (x + w * 0.5, y),
        Edge::Bottom => (x + w * 0.5, y + h),
        Edge::Left => (x, y + h * 0.5),
        Edge::Right => (x + w, y + h * 0.5),
    }
}

/// 补间值;过期的停在终值。
fn tween_value(tween: &Tween, time: f64) -> f32 {
    let progress = ((time - tween.start) / tween.over).clamp(0.0, 1.0);
    let shaped = match tween.ease {
        Some(index) => (expr::FUNCS[index].f)(&[f64::from(progress)], 0),
        None => f64::from(progress),
    };
    (tween.from + (tween.to - tween.from) * shaped as f32).clamp(0.0, 4.0)
}

#[allow(clippy::too_many_arguments)]
fn emit_nodes(
    scene: &Scene,
    view: &View<'_>,
    inputs: &Inputs<'_>,
    time: f64,
    nodes: &[Node],
    opacity_mult: f32,
    budget: &mut u32,
    degraded: &mut bool,
    items: &mut Vec<Item>,
) {
    for node in nodes {
        let visible = node.visible.as_ref().map_or(true, |expr| {
            eval(expr, scene, view.vars, inputs, budget, degraded) != 0.0
        });
        if !visible {
            continue;
        }
        let opacity = node.opacity.as_ref().map_or(1.0, |expr| {
            eval(expr, scene, view.vars, inputs, budget, degraded) as f32
        });
        let opacity = (opacity * opacity_mult).clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }
        match &node.body {
            NodeBody::Group { children } => {
                emit_nodes(
                    scene, view, inputs, time, children, opacity, budget, degraded, items,
                );
            }
            _ => {
                if let Some(item) =
                    emit_leaf(scene, view, inputs, time, node, opacity, budget, degraded)
                {
                    items.push(item);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_leaf(
    scene: &Scene,
    view: &View<'_>,
    inputs: &Inputs<'_>,
    time: f64,
    node: &Node,
    opacity: f32,
    budget: &mut u32,
    degraded: &mut bool,
) -> Option<Item> {
    let anchor = anchor_rect(&node.anchor, inputs)?;
    let effect = node.effect.as_ref().map(|effect| match effect {
        crate::model::Effect::Blur { radius } => EffectOut::Blur {
            radius: radius
                .as_ref()
                .map_or(4.0, |expr| {
                    eval(expr, scene, view.vars, inputs, budget, degraded) as f32
                })
                .max(0.0),
        },
        crate::model::Effect::Shadow { color, offset } => EffectOut::Shadow {
            color: *color,
            offset: *offset,
        },
    });

    let (rect, body) = match &node.body {
        NodeBody::Group { .. } => return None,
        NodeBody::Shape {
            shape,
            size,
            radius,
            stroke,
            fill,
        } => {
            let rect = match size {
                Some([w, h]) => centered(anchor, node, *w, *h),
                None => inset_rect(anchor, node.inset),
            };
            (
                shifted(rect, node.offset),
                Body::Shape {
                    shape: shape.clone(),
                    radius: *radius,
                    stroke: *stroke,
                    fill: *fill,
                },
            )
        }
        NodeBody::Image { src, size } => {
            let rect = match size {
                Some([w, h]) => centered(anchor, node, *w, *h),
                None => inset_rect(anchor, node.inset),
            };
            (shifted(rect, node.offset), Body::Image { src: src.clone() })
        }
        NodeBody::Sprite {
            src,
            grid,
            fps,
            frames,
            clips,
            follow,
        } => {
            // 播放中的片段优先;没播过用缺省片段;片段名对不上退回缺省。
            let (clip_name, elapsed) = view
                .sprites
                .get(&node.id)
                .map(|state| (state.clip.as_str(), state.time))
                .unwrap_or((frames.as_str(), 0.0));
            let clip = clips
                .get(clip_name)
                .or_else(|| clips.get(frames))
                .or_else(|| clips.values().next());
            let frame = clip
                .map(|clip| {
                    let index = if clip.looping {
                        (elapsed * f64::from(clip.fps)) as usize % clip.frames.len()
                    } else {
                        ((elapsed * f64::from(clip.fps)) as usize).min(clip.frames.len() - 1)
                    };
                    clip.frames[index]
                })
                .unwrap_or(0);
            let rect = match follow {
                Some(_) => {
                    // 弹簧追的是锚边点;矩形以它为中心落回帧上。
                    let position = view.springs.get(&node.id).copied().unwrap_or_else(|| {
                        let (cx, cy) = edge_point(anchor, node.anchor.edge);
                        [cx, cy]
                    });
                    let (w, h) = (f32::from(grid[0]), f32::from(grid[1]));
                    [position[0] - w * 0.5, position[1] - h * 0.5, w, h]
                }
                None => centered(anchor, node, f32::from(grid[0]), f32::from(grid[1])),
            };
            (
                shifted(rect, node.offset),
                Body::Sprite {
                    src: src.clone(),
                    frame,
                    grid: *grid,
                },
            )
        }
        NodeBody::Text {
            content,
            font_size,
            fill,
        } => {
            let rect = centered(anchor, node, anchor[2], f32::from(*font_size) * 1.6);
            (
                shifted(rect, node.offset),
                Body::Text {
                    content: content.clone(),
                    font_size: *font_size,
                    fill: *fill,
                },
            )
        }
        NodeBody::Emitter { shape, fill, .. } => {
            let particles = view
                .emitters
                .get(&node.id)
                .map(|emitter| {
                    emitter
                        .live
                        .iter()
                        .map(|live| Particle {
                            x: live.x,
                            y: live.y,
                            size: live.size,
                            opacity: (1.0 - live.age / live.life) as f32,
                        })
                        .collect()
                })
                .unwrap_or_default();
            (
                anchor,
                Body::Particles {
                    shape: shape.clone(),
                    fill: *fill,
                    particles,
                },
            )
        }
    };

    // 属性补间:opacity 与 offset 覆盖表达式的现值。
    let opacity = match active_tween(view.tweens, &node.id, TweenProp::Opacity) {
        Some(tween) => (tween_value(tween, time) * opacity).clamp(0.0, 1.0),
        None => opacity,
    };
    let rect = {
        let dx = active_tween(view.tweens, &node.id, TweenProp::OffsetX)
            .map_or(0.0, |tween| tween_value(tween, time));
        let dy = active_tween(view.tweens, &node.id, TweenProp::OffsetY)
            .map_or(0.0, |tween| tween_value(tween, time));
        [rect[0] + dx, rect[1] + dy, rect[2], rect[3]]
    };

    Some(Item {
        layer: node.layer,
        rect,
        opacity,
        effect,
        body,
    })
}

/// 该节点该属性的最新补间;没有就 None。
fn active_tween(tweens: &[Tween], node: &str, prop: TweenProp) -> Option<&Tween> {
    tweens
        .iter()
        .rev()
        .find(|tween| tween.node == node && tween.prop == prop)
}

/// 以锚边点为中心的矩形。
fn centered(anchor: &Rect, node: &Node, width: f32, height: f32) -> Rect {
    let (cx, cy) = edge_point(*anchor, node.anchor.edge);
    [cx - width * 0.5, cy - height * 0.5, width, height]
}

/// 锚矩形按 inset 收放;负值外扩。
fn inset_rect(rect: Rect, inset: f32) -> Rect {
    [
        rect[0] + inset,
        rect[1] + inset,
        (rect[2] - inset * 2.0).max(0.0),
        (rect[3] - inset * 2.0).max(0.0),
    ]
}

fn shifted(rect: Rect, offset: [f32; 2]) -> Rect {
    [rect[0] + offset[0], rect[1] + offset[1], rect[2], rect[3]]
}
