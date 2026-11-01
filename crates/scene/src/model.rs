//! 场景文件(`scene.toml`)的解析与校验。
//!
//! 全部是数据:信号变量、节点、规则。加载期把表达式编译成
//! [`expr::Expr`],名字对不上、字段用错位置、超出预算上限都在这里用
//! 中文报错(文件、行、节点、字段),一处都不放行——运行期不再有
//! 「半个场景」这种状态。

use std::collections::BTreeMap;

use serde::Deserialize;
use toml::Spanned;

use crate::expr::{self, Expr};

/// 内置信号名;表达式里可直接引用,只读。
pub const BUILTIN_SIGNALS: [&str; 7] = [
    "typing_rate",
    "candidates",
    "page",
    "page_count",
    "mode",
    "selected",
    "idle",
];

/// 节点、规则与变量的数量上限;场景是装饰,不是程序。
pub const MAX_NODES: usize = 64;
pub const MAX_RULES: usize = 64;
pub const MAX_VARS: usize = 32;

/// 加载错误;每条都带文件名,尽量带行号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "{} 第 {line} 行:{}", self.file, self.message),
            None => write!(f, "{}:{}", self.file, self.message),
        }
    }
}

/// 时长;TOML 里写 `"80ms"`、`"2s"`,裸数字按秒。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seconds(pub f64);

impl<'de> Deserialize<'de> for Seconds {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Seconds;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("时长:\"80ms\"、\"2s\" 或秒数")
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(Seconds(value))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(Seconds(value as f64))
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
                parse_duration(text)
                    .ok_or_else(|| E::custom(format!("不是时长:{text:?}(例:\"80ms\"、\"2s\")")))
                    .map(Seconds)
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

fn parse_duration(text: &str) -> Option<f64> {
    let text = text.trim();
    let (number, unit) = text.split_at(
        text.find(|c: char| c.is_ascii_alphabetic())
            .unwrap_or(text.len()),
    );
    let value: f64 = number.trim().parse().ok()?;
    let scale = match unit.trim().to_ascii_lowercase().as_str() {
        "s" => 1.0,
        "ms" => 0.001,
        _ => return None,
    };
    Some(value * scale)
}

/// 颜色:调色板名或 `#RRGGBB`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Fill {
    Palette(PaletteName),
    Rgba(u32),
}

/// 调色板名;与渲染核心的 `Palette` 字段一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PaletteName {
    Bg,
    Border,
    Text,
    TextSecondary,
    TextTertiary,
    Accent,
    OnAccent,
    Danger,
    AccentText,
    SelectedBg,
    SelectedText,
    Hover,
}

fn parse_fill(text: &str) -> Option<Fill> {
    let name = match text {
        "bg" => PaletteName::Bg,
        "border" => PaletteName::Border,
        "text" => PaletteName::Text,
        "text_secondary" => PaletteName::TextSecondary,
        "text_tertiary" => PaletteName::TextTertiary,
        "accent" => PaletteName::Accent,
        "on_accent" => PaletteName::OnAccent,
        "danger" => PaletteName::Danger,
        "accent_text" => PaletteName::AccentText,
        "selected_bg" => PaletteName::SelectedBg,
        "selected_text" => PaletteName::SelectedText,
        "hover" => PaletteName::Hover,
        _ => {
            let hex = text.strip_prefix('#')?;
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            return u32::from_str_radix(hex, 16).ok().map(Fill::Rgba);
        }
    };
    Some(Fill::Palette(name))
}

/// 节点锚:基准面 + 可选边。`candidate.N` 锚到页内第 N 条。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub base: AnchorBase,
    pub edge: Edge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorBase {
    Panel,
    Preedit,
    Highlight,
    Candidate(usize),
    Caret,
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Edge {
    #[default]
    Center,
    Top,
    Bottom,
    Left,
    Right,
}

fn parse_anchor(text: &str) -> Option<Anchor> {
    // candidate.N 先于边解析:锚里的点既可能是下标也可能是边。
    if let Some(rest) = text.strip_prefix("candidate.") {
        let (index, edge) = rest.split_once('.').unwrap_or((rest, ""));
        return Some(Anchor {
            base: AnchorBase::Candidate(index.parse().ok()?),
            edge: parse_edge(edge)?,
        });
    }
    let (base, edge) = match text.split_once('.') {
        Some((base, edge)) => (base, edge),
        None => (text, ""),
    };
    let base = match base {
        "panel" => AnchorBase::Panel,
        "preedit" => AnchorBase::Preedit,
        "highlight" => AnchorBase::Highlight,
        "caret" => AnchorBase::Caret,
        "screen" => AnchorBase::Screen,
        _ => return None,
    };
    Some(Anchor {
        base,
        edge: parse_edge(edge)?,
    })
}

fn parse_edge(edge: &str) -> Option<Edge> {
    match edge {
        "" | "center" => Some(Edge::Center),
        "top" => Some(Edge::Top),
        "bottom" => Some(Edge::Bottom),
        "left" => Some(Edge::Left),
        "right" => Some(Edge::Right),
        _ => None,
    }
}

/// 节点画在文字下方还是上方;文字永远最后画,装饰压不坏候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub enum Layer {
    #[default]
    BelowText,
    AboveText,
}

/// 几何形状;`path` 是 M/L/Q/C/Z 子集,坐标相对锚点。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Shape {
    Rect,
    RRect,
    Ellipse,
    Line { to: [f32; 2] },
    Path(Vec<PathSeg>),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum PathSeg {
    MoveTo([f32; 2]),
    LineTo([f32; 2]),
    QuadTo {
        control: [f32; 2],
        to: [f32; 2],
    },
    CubicTo {
        c1: [f32; 2],
        c2: [f32; 2],
        to: [f32; 2],
    },
    Close,
}

/// D2D 内置特效;运行期把半径等表达式求成数,由窗口层接成特效图。
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Blur {
        radius: Option<Expr>,
    },
    Shadow {
        color: Option<Fill>,
        offset: [f32; 2],
    },
}

/// 精灵片段:帧序列、帧率与是否循环。
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub frames: Vec<u32>,
    pub fps: f32,
    pub looping: bool,
}

/// 精灵节点的跟随参数;`follow` 是每秒的追随速率,缺省硬贴锚点。
#[derive(Debug, Clone, PartialEq)]
pub struct Follow {
    pub spring: f32,
}

/// 节点本体;kind 决定哪些字段有意义。
#[derive(Debug, Clone, PartialEq)]
pub enum NodeBody {
    Shape {
        shape: Shape,
        size: Option<[f32; 2]>,
        radius: f32,
        stroke: f32,
        fill: Fill,
    },
    Image {
        src: String,
        size: Option<[f32; 2]>,
    },
    Sprite {
        src: String,
        grid: [u32; 2],
        fps: f32,
        frames: String,
        clips: BTreeMap<String, Clip>,
        follow: Option<Follow>,
    },
    Text {
        content: String,
        font_size: f32,
        fill: Fill,
    },
    Group {
        children: Vec<Node>,
    },
    Emitter {
        shape: Shape,
        rate: Expr,
        life: f64,
        speed: [f32; 2],
        direction: f32,
        spread: f32,
        gravity: f32,
        size: [f32; 2],
        fill: Fill,
    },
}

/// 一个场景节点。
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub body: NodeBody,
    pub anchor: Anchor,
    pub inset: f32,
    pub offset: [f32; 2],
    pub layer: Layer,
    pub visible: Option<Expr>,
    pub opacity: Option<Expr>,
    pub effect: Option<Effect>,
}

/// 可被补间的节点属性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TweenProp {
    Opacity,
    OffsetX,
    OffsetY,
}

impl TweenProp {
    fn parse(text: &str) -> Option<(String, Self)> {
        let (node, prop) = text.split_once('.')?;
        let prop = match prop {
            "opacity" => Self::Opacity,
            "offset.x" => Self::OffsetX,
            "offset.y" => Self::OffsetY,
            _ => return None,
        };
        Some((node.to_owned(), prop))
    }
}

/// 补间缓动;存内置函数表的下标,求值与表达式共用一份曲线。
pub type Ease = usize;

/// 规则动作;`then` 让延时后的动作链继续。
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Set {
        name: String,
        to: Expr,
    },
    Tween {
        node: String,
        prop: TweenProp,
        to: Expr,
        over: f64,
        ease: Option<Ease>,
    },
    Play {
        node: String,
        clip: String,
    },
    Emit {
        event: String,
    },
    Delay {
        after: f64,
        then: Vec<Action>,
    },
}

/// 事件触发的一条规则。
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub on: String,
    pub when: Option<Expr>,
    pub actions: Vec<Action>,
}

/// 用户变量;`from` 驱动的会自动升与衰减,纯规则变量省略 `from`。
#[derive(Debug, Clone, PartialEq)]
pub struct VarDecl {
    pub name: String,
    pub from: Option<Expr>,
    /// 上升速率:每秒逼近剩余差距的比例。
    pub gain: f64,
    /// 衰减时间常数(秒)。
    pub decay: f64,
}

/// 解析好的场景;`seed` 让 rand 对每个文件稳定。
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    pub vars: Vec<VarDecl>,
    pub nodes: Vec<Node>,
    pub rules: Vec<Rule>,
    pub seed: u64,
}

// ---- TOML 原始形态;Spanned 拿行号,deny_unknown_fields 挡拼错 ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScene {
    #[serde(default)]
    signals: BTreeMap<String, Spanned<RawSignal>>,
    #[serde(default)]
    nodes: Vec<Spanned<RawNode>>,
    #[serde(default)]
    rules: Vec<Spanned<RawRule>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSignal {
    from: Option<String>,
    gain: Option<f64>,
    decay: Option<Seconds>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEffect {
    kind: String,
    radius: Option<String>,
    color: Option<String>,
    offset: Option<[f32; 2]>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClip {
    frames: Vec<u32>,
    fps: Option<f32>,
    #[serde(rename = "loop")]
    looping: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFollow {
    spring: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNode {
    id: Spanned<String>,
    kind: String,
    anchor: Option<String>,
    inset: Option<f32>,
    offset: Option<[f32; 2]>,
    layer: Option<String>,
    visible: Option<String>,
    opacity: Option<String>,
    effect: Option<RawEffect>,
    shape: Option<String>,
    size: Option<[f32; 2]>,
    radius: Option<f32>,
    stroke: Option<f32>,
    fill: Option<String>,
    points: Option<[[f32; 2]; 2]>,
    d: Option<String>,
    src: Option<String>,
    grid: Option<[u32; 2]>,
    fps: Option<f32>,
    frames: Option<String>,
    clips: Option<BTreeMap<String, RawClip>>,
    follow: Option<RawFollow>,
    content: Option<String>,
    font_size: Option<f32>,
    rate: Option<String>,
    life: Option<Seconds>,
    speed: Option<[f32; 2]>,
    direction: Option<f32>,
    spread: Option<f32>,
    gravity: Option<f32>,
    children: Option<Vec<Spanned<RawNode>>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    on: Spanned<String>,
    when: Option<String>,
    #[serde(default, rename = "do")]
    actions: Vec<Spanned<toml::Table>>,
}

// ---- 构建:原始形态 → 编译好的场景 ----

struct Builder<'a> {
    file: &'a str,
    lines: Vec<usize>,
    errors: Vec<LoadError>,
    vars: Vec<VarDecl>,
    /// 见过的节点总数(含 group 子节点);上限双倍于顶层节点。
    nodes_seen: usize,
}

impl Builder<'_> {
    /// 字节偏移 → 1 起的行号。
    fn line(&self, byte: usize) -> usize {
        match self.lines.binary_search(&byte) {
            Ok(index) => index + 1,
            Err(index) => index,
        }
    }

    fn error(&mut self, byte: usize, message: impl Into<String>) {
        self.errors.push(LoadError {
            file: self.file.to_owned(),
            line: Some(self.line(byte)),
            message: message.into(),
        });
    }

    fn expr(&mut self, text: &str, byte: usize, context: &str) -> Option<Expr> {
        match expr::parse(text) {
            Ok(parsed) => {
                let mut names = Vec::new();
                parsed.names(&mut names);
                for name in names {
                    let known = self.vars.iter().any(|var| var.name == name)
                        || BUILTIN_SIGNALS.contains(&name.as_str());
                    if !known {
                        self.error(
                            byte,
                            format!("{context} 引用了没有的信号 {name:?};内置信号有 {BUILTIN_SIGNALS:?},用户变量要写在 [signals.*] 里"),
                        );
                    }
                }
                Some(parsed)
            }
            Err(error) => {
                self.error(
                    byte,
                    format!("{context} 的表达式 {text:?} 解析失败:{error}"),
                );
                None
            }
        }
    }

    fn fill(&mut self, text: &str, byte: usize, context: &str) -> Option<Fill> {
        match parse_fill(text) {
            Some(fill) => Some(fill),
            None => {
                self.error(
                    byte,
                    format!(
                        "{context} 不认识的颜色 {text:?};写调色板名(accent、bg、text…)或 #RRGGBB"
                    ),
                );
                None
            }
        }
    }
}

/// 解析并校验场景;所有问题一次性报出来,修一处不用再跑一轮。
pub fn parse(file: &str, text: &str) -> Result<Scene, Vec<LoadError>> {
    let mut lines: Vec<usize> = text
        .bytes()
        .enumerate()
        .filter(|&(_, byte)| byte == b'\n')
        .map(|(offset, _)| offset + 1)
        .collect();
    lines.insert(0, 0);
    let raw: RawScene = match toml::from_str(text) {
        Ok(raw) => raw,
        Err(error) => {
            let line = error
                .span()
                .map(|span| match lines.binary_search(&span.start) {
                    Ok(index) => index + 1,
                    Err(index) => index,
                });
            return Err(vec![LoadError {
                file: file.to_owned(),
                line,
                message: format!("TOML 解析失败:{error}"),
            }]);
        }
    };
    let mut builder = Builder {
        file,
        lines,
        errors: Vec::new(),
        vars: Vec::new(),
        nodes_seen: 0,
    };
    let seed = fnv1a(text.as_bytes());

    // 变量先建表,后面表达式才知道哪些名字存在。
    for (name, signal) in &raw.signals {
        if builder.vars.len() >= MAX_VARS {
            builder.error(
                signal.span().start,
                format!("用户变量超过 {MAX_VARS} 个,场景不做状态机"),
            );
            break;
        }
        let span = signal.span().start;
        let signal = signal.get_ref();
        let from = signal
            .from
            .as_ref()
            .and_then(|text| builder.expr(text, span, &format!("信号 {name:?} 的 from")));
        builder.vars.push(VarDecl {
            name: name.clone(),
            from,
            gain: signal.gain.unwrap_or(1.0).max(0.0),
            decay: signal
                .decay
                .map_or(1.0, |Seconds(seconds)| seconds.max(0.01)),
        });
    }

    let mut nodes = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for raw_node in &raw.nodes {
        build_node(&mut builder, raw_node, &mut nodes, &mut ids, 0);
    }
    let rules = build_rules(&mut builder, &raw.rules);

    if !builder.errors.is_empty() {
        return Err(builder.errors);
    }
    Ok(Scene {
        vars: builder.vars,
        nodes,
        rules,
        seed,
    })
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// kind 对哪些字段有意义;拼错的字段名 deny_unknown_fields 挡住,
/// 用对名字但放错 kind 的在这里挡住。
fn allowed_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "shape" => &["shape", "size", "radius", "stroke", "fill"],
        "image" => &["src", "size"],
        "sprite" => &["src", "grid", "fps", "frames", "clips", "follow"],
        "text" => &["content", "font_size", "fill"],
        "group" => &["children"],
        "emitter" => &[
            "shape",
            "rate",
            "life",
            "speed",
            "direction",
            "spread",
            "gravity",
            "size",
            "fill",
        ],
        _ => &[],
    }
}

fn build_node(
    builder: &mut Builder<'_>,
    raw: &Spanned<RawNode>,
    out: &mut Vec<Node>,
    ids: &mut Vec<String>,
    depth: usize,
) {
    let byte = raw.span().start;
    let raw = raw.get_ref();
    builder.nodes_seen += 1;
    if builder.nodes_seen > MAX_NODES * 2 {
        builder.error(
            byte,
            format!("节点总数(含 group 子节点)超过 {},场景太重了", MAX_NODES * 2),
        );
        return;
    }
    if depth > 4 {
        builder.error(byte, "group 嵌套超过 4 层");
        return;
    }
    let id = raw.id.get_ref().clone();
    if depth == 0 {
        if ids.iter().any(|existing| existing == &id) {
            builder.error(raw.id.span().start, format!("节点 id {id:?} 重复"));
        }
        ids.push(id.clone());
    }
    let context = format!("节点 {id:?}");

    let known = allowed_fields(&raw.kind);
    for (name, set) in [
        ("shape", raw.shape.is_some()),
        ("size", raw.size.is_some()),
        ("radius", raw.radius.is_some()),
        ("stroke", raw.stroke.is_some()),
        ("fill", raw.fill.is_some()),
        ("points", raw.points.is_some()),
        ("d", raw.d.is_some()),
        ("src", raw.src.is_some()),
        ("grid", raw.grid.is_some()),
        ("fps", raw.fps.is_some()),
        ("frames", raw.frames.is_some()),
        ("clips", raw.clips.is_some()),
        ("content", raw.content.is_some()),
        ("font_size", raw.font_size.is_some()),
        ("rate", raw.rate.is_some()),
        ("life", raw.life.is_some()),
        ("speed", raw.speed.is_some()),
        ("direction", raw.direction.is_some()),
        ("spread", raw.spread.is_some()),
        ("gravity", raw.gravity.is_some()),
        ("children", raw.children.is_some()),
    ] {
        if set && !known.contains(&name) {
            builder.error(
                byte,
                format!("{context}(kind={}) 用不到字段 {name:?}", raw.kind),
            );
        }
    }

    let anchor = raw.anchor.as_ref().map_or(
        Ok(Anchor { base: AnchorBase::Panel, edge: Edge::Center }),
        |text| {
            parse_anchor(text).ok_or_else(|| {
                format!(
                    "{context} 不认识的锚 {text:?};写 panel/preedit/highlight/candidate.N/caret/screen,可带 .top/.bottom/.left/.right"
                )
            })
        },
    );
    let layer = raw
        .layer
        .as_ref()
        .map_or(Ok(Layer::BelowText), |text| match text.as_str() {
            "below-text" => Ok(Layer::BelowText),
            "above-text" => Ok(Layer::AboveText),
            _ => Err(format!(
                "{context} 不认识的层 {text:?};写 below-text 或 above-text"
            )),
        });
    let visible = raw
        .visible
        .as_ref()
        .and_then(|text| builder.expr(text, byte, &context));
    let opacity = raw
        .opacity
        .as_ref()
        .and_then(|text| builder.expr(text, byte, &context));
    let effect = raw.effect.as_ref().and_then(|raw| {
        let effect = match raw.kind.as_str() {
            "blur" => Effect::Blur {
                radius: raw
                    .radius
                    .as_ref()
                    .and_then(|text| builder.expr(text, byte, &format!("{context} 的模糊半径"))),
            },
            "shadow" => Effect::Shadow {
                color: raw
                    .color
                    .as_ref()
                    .and_then(|text| builder.fill(text, byte, &context)),
                offset: raw.offset.unwrap_or([0.0, 4.0]),
            },
            other => {
                builder.error(
                    byte,
                    format!("{context} 不认识的特效 {other:?};v1 支持 blur 与 shadow"),
                );
                return None;
            }
        };
        Some(effect)
    });
    let (anchor, layer) = match (anchor, layer) {
        (Ok(anchor), Ok(layer)) => (anchor, layer),
        (Err(message), _) | (_, Err(message)) => {
            builder.error(byte, message);
            return;
        }
    };

    let body = match raw.kind.as_str() {
        "shape" => {
            let shape = match raw.shape.as_deref().unwrap_or("rect") {
                "rect" => Shape::Rect,
                "rrect" => Shape::RRect,
                "ellipse" => Shape::Ellipse,
                "line" => Shape::Line {
                    to: raw.points.map_or([0.0, 0.0], |points| points[1]),
                },
                "path" => {
                    let Some(d) = raw.d.as_deref() else {
                        builder.error(byte, format!("{context} 的 path 要写 d 字段"));
                        return;
                    };
                    match parse_path(d) {
                        Ok(segments) if !segments.is_empty() => Shape::Path(segments),
                        Ok(_) => {
                            builder.error(byte, format!("{context} 的 d 是空的"));
                            return;
                        }
                        Err(message) => {
                            builder.error(byte, format!("{context} 的 d 解析失败:{message}"));
                            return;
                        }
                    }
                }
                other => {
                    builder.error(
                        byte,
                        format!("{context} 不认识的形状 {other:?};写 rect/rrect/ellipse/line/path"),
                    );
                    return;
                }
            };
            let fill = raw
                .fill
                .as_deref()
                .map_or(Fill::Palette(PaletteName::Accent), |text| {
                    builder
                        .fill(text, byte, &context)
                        .unwrap_or(Fill::Palette(PaletteName::Accent))
                });
            NodeBody::Shape {
                shape,
                size: raw.size,
                radius: raw.radius.unwrap_or(0.0),
                stroke: raw.stroke.unwrap_or(0.0),
                fill,
            }
        }
        "image" => {
            let Some(src) = raw.src.clone() else {
                builder.error(byte, format!("{context} 要写 src"));
                return;
            };
            NodeBody::Image {
                src,
                size: raw.size,
            }
        }
        "sprite" => {
            let (Some(src), Some(grid)) = (raw.src.clone(), raw.grid) else {
                builder.error(byte, format!("{context} 要写 src 与 grid(图集帧尺寸)"));
                return;
            };
            let mut clips = BTreeMap::new();
            if let Some(raw_clips) = &raw.clips {
                for (name, clip) in raw_clips {
                    if clip.frames.is_empty() {
                        builder.error(byte, format!("{context} 的片段 {name:?} 没有帧"));
                        continue;
                    }
                    clips.insert(
                        name.clone(),
                        Clip {
                            frames: clip.frames.clone(),
                            fps: clip.fps.unwrap_or(8.0).max(0.1),
                            looping: clip.looping.unwrap_or(true),
                        },
                    );
                }
            }
            let frames = raw.frames.clone().unwrap_or_else(|| {
                clips
                    .keys()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "idle".into())
            });
            let follow = raw.follow.as_ref().map(|follow| Follow {
                spring: follow.spring.max(0.1),
            });
            NodeBody::Sprite {
                src,
                grid,
                fps: raw.fps.unwrap_or(8.0).max(0.1),
                frames,
                clips,
                follow,
            }
        }
        "text" => {
            let Some(content) = raw.content.clone() else {
                builder.error(
                    byte,
                    format!("{context} 要写 content;场景拿不到候选文字,文字只能是场景自己的"),
                );
                return;
            };
            NodeBody::Text {
                content,
                font_size: raw.font_size.unwrap_or(14.0),
                fill: raw
                    .fill
                    .as_deref()
                    .and_then(|text| builder.fill(text, byte, &context))
                    .unwrap_or(Fill::Palette(PaletteName::Text)),
            }
        }
        "group" => {
            let mut children = Vec::new();
            if let Some(raw_children) = &raw.children {
                for child in raw_children {
                    build_node(builder, child, &mut children, &mut Vec::new(), depth + 1);
                }
            }
            NodeBody::Group { children }
        }
        "emitter" => {
            let shape = match raw.shape.as_deref().unwrap_or("rect") {
                "rect" => Shape::Rect,
                "rrect" => Shape::RRect,
                "ellipse" => Shape::Ellipse,
                other => {
                    builder.error(
                        byte,
                        format!("{context} 的粒子形状 {other:?} 不支持;写 rect/rrect/ellipse"),
                    );
                    return;
                }
            };
            let rate = match raw.rate.as_deref() {
                Some(text) => match builder.expr(text, byte, &format!("{context} 的 rate")) {
                    Some(parsed) => parsed,
                    None => return,
                },
                None => Expr::Num(10.0),
            };
            let speed = raw.speed.unwrap_or([10.0, 40.0]);
            let fill = raw
                .fill
                .as_deref()
                .map_or(Fill::Palette(PaletteName::Accent), |text| {
                    builder
                        .fill(text, byte, &context)
                        .unwrap_or(Fill::Palette(PaletteName::Accent))
                });
            NodeBody::Emitter {
                shape,
                rate,
                life: raw.life.map_or(1.0, |Seconds(seconds)| seconds.max(0.05)),
                speed: [speed[0].max(0.0), speed[1].max(speed[0])],
                direction: raw.direction.unwrap_or(90.0),
                spread: raw.spread.unwrap_or(30.0).clamp(0.0, 180.0),
                gravity: raw.gravity.unwrap_or(0.0),
                size: {
                    let size = raw.size.unwrap_or([2.0, 4.0]);
                    [size[0].max(0.5), size[1].max(size[0])]
                },
                fill,
            }
        }
        other => {
            builder.error(
                byte,
                format!(
                    "{context} 不认识的 kind {other:?};写 shape/image/sprite/text/group/emitter"
                ),
            );
            return;
        }
    };

    out.push(Node {
        id,
        body,
        anchor,
        inset: raw.inset.unwrap_or(0.0),
        offset: raw.offset.unwrap_or([0.0, 0.0]),
        layer,
        visible,
        opacity,
        effect,
    });
}

fn build_rules(builder: &mut Builder<'_>, raw: &[Spanned<RawRule>]) -> Vec<Rule> {
    let mut rules = Vec::new();
    for rule in raw {
        let byte = rule.span().start;
        if rules.len() >= MAX_RULES {
            builder.error(byte, format!("规则超过 {MAX_RULES} 条"));
            break;
        }
        let rule = rule.get_ref();
        let on = rule.on.get_ref();
        let context = format!("规则 on={on:?}");
        let when = rule
            .when
            .as_ref()
            .and_then(|text| builder.expr(text, byte, &context));
        let actions: Vec<(usize, &toml::Table)> = rule
            .actions
            .iter()
            .map(|action| (action.span().start, action.get_ref()))
            .collect();
        let mut actions_built = Vec::new();
        build_actions(builder, &actions, &mut actions_built, 0, &context);
        rules.push(Rule {
            on: on.clone(),
            when,
            actions: actions_built,
        });
    }
    rules
}

/// 构建动作;(行号, 表) 让 `then` 里的嵌套动作也能报到行。
fn build_actions(
    builder: &mut Builder<'_>,
    raw: &[(usize, &toml::Table)],
    out: &mut Vec<Action>,
    depth: usize,
    context: &str,
) {
    if depth > 4 {
        builder.error(0, format!("{context} 的动作链嵌套超过 4 层"));
        return;
    }
    for &(byte, table) in raw {
        let count = ["set", "tween", "play", "emit", "delay"]
            .into_iter()
            .filter(|name| table.contains_key(*name))
            .count();
        if count != 1 {
            builder.error(
                byte,
                format!("{context} 的动作要恰好一个 set/tween/play/emit/delay,这里有 {count} 个"),
            );
            continue;
        }
        let set = |name: &str| table.contains_key(name);
        if set("set") {
            build_set(builder, table, byte, out, context);
        } else if set("tween") {
            build_tween(builder, table, byte, out, context);
        } else if set("play") {
            match table["play"].as_str() {
                Some(text) => match text.split_once('.') {
                    Some((node, clip)) if !node.is_empty() && !clip.is_empty() => {
                        out.push(Action::Play {
                            node: node.to_owned(),
                            clip: clip.to_owned(),
                        });
                    }
                    _ => builder.error(
                        byte,
                        format!("{context} 的 play 要写 \"节点.片段\",例如 \"mochi.cheer\""),
                    ),
                },
                None => builder.error(byte, format!("{context} 的 play 要是字符串")),
            }
        } else if set("emit") {
            match table["emit"].as_str() {
                Some(event) if !event.is_empty() => {
                    out.push(Action::Emit {
                        event: event.to_owned(),
                    });
                }
                _ => builder.error(byte, format!("{context} 的 emit 要是非空字符串")),
            }
        } else {
            build_delay(builder, table, byte, out, depth, context);
        }
    }
}

fn build_set(
    builder: &mut Builder<'_>,
    table: &toml::Table,
    byte: usize,
    out: &mut Vec<Action>,
    context: &str,
) {
    let Some(name) = table["set"].as_str() else {
        builder.error(byte, format!("{context} 的 set 要是变量名"));
        return;
    };
    if !builder.vars.iter().any(|var| var.name == name) {
        builder.error(
            byte,
            format!("{context} 的 set 写了没有的变量 {name:?};先在 [signals.{name}] 里声明"),
        );
        return;
    }
    let to = match parse_action_value(table, "to", byte, context, builder) {
        Some(expr) => expr,
        None => return,
    };
    out.push(Action::Set {
        name: name.to_owned(),
        to,
    });
}

fn build_tween(
    builder: &mut Builder<'_>,
    table: &toml::Table,
    byte: usize,
    out: &mut Vec<Action>,
    context: &str,
) {
    let Some(target) = table["tween"].as_str().and_then(TweenProp::parse) else {
        builder.error(
            byte,
            format!("{context} 的 tween 要写 \"节点.属性\",属性是 opacity、offset.x 或 offset.y"),
        );
        return;
    };
    let to = match parse_action_value(table, "to", byte, context, builder) {
        Some(expr) => expr,
        None => return,
    };
    let over = match table.get("over") {
        Some(toml::Value::String(text)) => parse_duration(text),
        Some(toml::Value::Integer(value)) => Some(*value as f64),
        _ => None,
    };
    let Some(over) = over else {
        builder.error(
            byte,
            format!("{context} 的 tween 要写 over,例如 over = \"80ms\""),
        );
        return;
    };
    let ease = table
        .get("ease")
        .and_then(|value| value.as_str())
        .map(|name| {
            expr::FUNCS
                .iter()
                .position(|func| func.name == name && func.arity == 1)
        });
    if let Some(None) = ease {
        builder.error(
            byte,
            format!(
                "{context} 的缓动不认识;可用 linear/smooth/out_quad/out_cubic/out_quint/out_back"
            ),
        );
    }
    out.push(Action::Tween {
        node: target.0,
        prop: target.1,
        to,
        over: over.max(0.01),
        ease: ease.flatten(),
    });
}

fn build_delay(
    builder: &mut Builder<'_>,
    table: &toml::Table,
    byte: usize,
    out: &mut Vec<Action>,
    depth: usize,
    context: &str,
) {
    let after = match table.get("delay") {
        Some(toml::Value::String(text)) => parse_duration(text),
        Some(toml::Value::Integer(value)) => Some(*value as f64),
        _ => None,
    };
    let Some(after) = after else {
        builder.error(
            byte,
            format!("{context} 的 delay 要写时长,例如 delay = \"500ms\""),
        );
        return;
    };
    let mut then = Vec::new();
    if let Some(toml::Value::Array(items)) = table.get("then") {
        // 数组里的内联表拿不到独立行号;行号统一记在 delay 这一行。
        for item in items {
            match item.as_table() {
                Some(table) => {
                    build_actions(builder, &[(byte, table)], &mut then, depth + 1, context)
                }
                None => builder.error(byte, format!("{context} 的 then 里要全是动作表")),
            }
        }
    }
    out.push(Action::Delay { after, then });
}

/// 动作里的 `to`:数字或表达式字符串。
fn parse_action_value(
    table: &toml::Table,
    key: &str,
    byte: usize,
    context: &str,
    builder: &mut Builder<'_>,
) -> Option<Expr> {
    match table.get(key) {
        Some(toml::Value::Float(value)) => Some(Expr::Num(*value)),
        Some(toml::Value::Integer(value)) => Some(Expr::Num(*value as f64)),
        Some(toml::Value::String(text)) => builder.expr(text, byte, &format!("{context} 的 {key}")),
        _ => {
            builder.error(byte, format!("{context} 的 {key} 要是数或表达式"));
            None
        }
    }
}

/// 解析 path 的 `d`:M/L/Q/C/Z 绝对坐标子集,逗号或空白分隔。
/// 数字可连写(`-2.5-3` 是两个数);同命令多组参数按 SVG 语义重复,
/// `M` 的后续组按 `L` 处理。
fn parse_path(d: &str) -> Result<Vec<PathSeg>, String> {
    #[derive(Debug, Clone, Copy)]
    enum Token {
        Cmd(char),
        Num(f32),
    }
    let chars: Vec<char> = d.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() || ch == ',' {
            index += 1;
            continue;
        }
        if ch.is_ascii_alphabetic() {
            tokens.push(Token::Cmd(ch.to_ascii_uppercase()));
            index += 1;
            continue;
        }
        let start = index;
        if matches!(ch, '-' | '+') {
            index += 1;
        }
        while index < chars.len() && (chars[index].is_ascii_digit() || chars[index] == '.') {
            index += 1;
        }
        let literal: String = chars[start..index].iter().collect();
        let value: f32 = literal
            .parse()
            .map_err(|_| format!("不是合法的数:{literal:?}"))?;
        tokens.push(Token::Num(value));
    }

    fn arity(command: char) -> Option<usize> {
        match command {
            'M' | 'L' => Some(2),
            'Q' => Some(4),
            'C' => Some(6),
            'Z' => Some(0),
            _ => None,
        }
    }
    fn flush(
        command: char,
        pending: &mut Vec<f32>,
        segments: &mut Vec<PathSeg>,
    ) -> Result<(), String> {
        let size = arity(command).ok_or_else(|| format!("不认识的路径命令 {command:?}"))?;
        if size == 0 {
            if !pending.is_empty() {
                return Err("Z 不带参数".into());
            }
            segments.push(PathSeg::Close);
            return Ok(());
        }
        if pending.is_empty() || !pending.len().is_multiple_of(size) {
            return Err(format!("{command} 的参数个数要是 {size} 的倍数"));
        }
        let mut numbers = pending.drain(..);
        let mut first = true;
        while let Some(value) = numbers.next() {
            let group: Vec<f32> = std::iter::once(value)
                .chain(numbers.by_ref().take(size - 1))
                .collect();
            segments.push(match command {
                'M' if first => PathSeg::MoveTo([group[0], group[1]]),
                // M 的后续组是 L;Q 与 C 的多组参数重复自己。
                'M' | 'L' => PathSeg::LineTo([group[0], group[1]]),
                'Q' => PathSeg::QuadTo {
                    control: [group[0], group[1]],
                    to: [group[2], group[3]],
                },
                'C' => PathSeg::CubicTo {
                    c1: [group[0], group[1]],
                    c2: [group[2], group[3]],
                    to: [group[4], group[5]],
                },
                _ => unreachable!("arity 只认 M/L/Q/C/Z"),
            });
            first = false;
        }
        Ok(())
    }

    let mut segments = Vec::new();
    let mut command: Option<char> = None;
    let mut pending: Vec<f32> = Vec::new();
    for token in tokens {
        match token {
            Token::Cmd(next) => {
                if let Some(done) = command.replace(next) {
                    // Z 后面直接跟新命令;数字都归前一个命令。
                    if done != 'Z' || pending.is_empty() {
                        flush(done, &mut pending, &mut segments)?;
                    }
                }
            }
            Token::Num(value) => {
                let Some(done) = command else {
                    return Err("路径要以 M 开头".into());
                };
                arity(done).ok_or_else(|| format!("不认识的路径命令 {done:?}"))?;
                pending.push(value);
            }
        }
    }
    if let Some(done) = command {
        flush(done, &mut pending, &mut segments)?;
    }
    if !matches!(segments.first(), Some(PathSeg::MoveTo(_))) {
        return Err("路径要以 M 开头".into());
    }
    Ok(segments)
}
