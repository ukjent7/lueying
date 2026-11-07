//! 动画包的正式格式:一个目录,里面一份 `pack.json` 和若干帧图。
//!
//! 包里写清楚四件事:帧图在哪、动画片段有哪些(每帧时长与帧号)、角色有几个、画在哪一层,
//! 以及角色的行为。行为可以直接引用预设(`strip-cat`、`spider`、`warrior`),也可以把状态与
//! 转移写在包里,扩展作者不用写代码。解析与校验都在这里,出错的地方会报出是哪一项。
//!
//! 格式版本固定为 1。帧图的文件名是 `<帧号>.png`,放在 `frames` 指向的目录里(默认就是包目录)。

use lueying_render::actor::{
    Axes, Behavior, StateDef, Transition, Trigger, Walk, spider, strip_cat, warrior,
};
use lueying_render::animation::{Clip, Pack};
use serde::Deserialize;
use std::collections::BTreeMap;

/// 当前支持的格式版本。
pub const FORMAT: u32 = 1;
/// 一个包里角色的数量上限;防止写错一个数字把窗口卡死。
pub const MAX_ACTORS: usize = 256;

/// 解析好的动画包。
#[derive(Clone, Debug)]
pub struct SkinPack {
    pub name: String,
    /// 帧图所在目录,相对于包目录;默认是包目录本身。
    pub frames: String,
    pub pack: Pack,
    pub actors: Vec<ActorSpec>,
}

/// 一组角色:数量、画在哪一层、行为。
#[derive(Clone, Debug)]
pub struct ActorSpec {
    pub count: usize,
    pub layer: Layer,
    pub behavior: Behavior,
}

/// 角色画在按钮的下面还是上面。由扩展自己权衡,这里只是记下来。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Below,
    Above,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackFile {
    format: u32,
    name: String,
    #[serde(default = "default_frames")]
    frames: String,
    clips: BTreeMap<String, ClipDef>,
    #[serde(default)]
    actors: Vec<ActorFile>,
}

fn default_frames() -> String {
    ".".to_string()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipDef {
    ms: u64,
    frames: Vec<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActorFile {
    #[serde(default = "one")]
    count: usize,
    #[serde(default = "default_layer")]
    layer: LayerDef,
    behavior: BehaviorRef,
}

fn one() -> usize {
    1
}

fn default_layer() -> LayerDef {
    LayerDef::Below
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum LayerDef {
    Below,
    Above,
}

/// 行为可以是预设的名字,也可以是写在包里的完整定义。
#[derive(Deserialize)]
#[serde(untagged)]
enum BehaviorRef {
    Preset(String),
    Inline(BehaviorDef),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorDef {
    #[serde(default)]
    start: usize,
    states: Vec<StateFile>,
    #[serde(default)]
    transitions: Vec<TransitionFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    clip: String,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    walk: Option<WalkFile>,
    #[serde(default)]
    duration: Option<(u64, u64)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WalkFile {
    speed: f32,
    axes: AxesFile,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum AxesFile {
    Horizontal,
    Free,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionFile {
    from: usize,
    to: usize,
    on: TriggerFile,
    #[serde(default = "one_weight")]
    weight: u32,
}

fn one_weight() -> u32 {
    1
}

/// 触发条件:`"timeout"`、`"key"`、`"edge"`、`"click"`,或 `{"quiet": 毫秒}`、`{"near": {...}}`。
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum TriggerFile {
    Timeout,
    Key,
    Edge,
    Click,
    Quiet(u64),
    Near(NearFile),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NearFile {
    range: f32,
    tag: String,
}

/// 解析并校验一份 `pack.json`。
pub fn parse(text: &str) -> Result<SkinPack, String> {
    let file: PackFile =
        serde_json::from_str(text).map_err(|e| format!("pack.json 解析失败: {e}"))?;
    if file.format != FORMAT {
        return Err(format!(
            "不支持的格式版本 {},当前只认 {FORMAT}",
            file.format
        ));
    }
    if file.name.trim().is_empty() {
        return Err("动画包要有名字".into());
    }
    let mut clips = Vec::new();
    for (name, clip) in &file.clips {
        if clip.ms == 0 {
            return Err(format!("片段 {name} 的每帧毫秒必须大于 0"));
        }
        if clip.frames.is_empty() {
            return Err(format!("片段 {name} 没有帧"));
        }
        clips.push((
            name.clone(),
            Clip {
                frames: clip.frames.clone(),
                frame_ms: clip.ms,
            },
        ));
    }
    let pack = Pack::new(clips);
    let mut actors = Vec::with_capacity(file.actors.len());
    for (index, actor) in file.actors.into_iter().enumerate() {
        if actor.count == 0 || actor.count > MAX_ACTORS {
            return Err(format!(
                "第 {} 组角色数量要在 1–{MAX_ACTORS} 之间",
                index + 1
            ));
        }
        let behavior = match actor.behavior {
            BehaviorRef::Preset(name) => preset(&name)
                .ok_or_else(|| format!("第 {} 组角色引用了不存在的预设 {name}", index + 1))?,
            BehaviorRef::Inline(def) => behavior_of(def)?,
        };
        behavior
            .validate()
            .map_err(|e| format!("第 {} 组角色的行为不合法: {e}", index + 1))?;
        for state in &behavior.states {
            if pack.clip(&state.clip).is_none() {
                return Err(format!(
                    "第 {} 组角色的行为引用了不存在的片段 {}",
                    index + 1,
                    state.clip
                ));
            }
        }
        actors.push(ActorSpec {
            count: actor.count,
            layer: match actor.layer {
                LayerDef::Below => Layer::Below,
                LayerDef::Above => Layer::Above,
            },
            behavior,
        });
    }
    Ok(SkinPack {
        name: file.name,
        frames: file.frames,
        pack,
        actors,
    })
}

/// 预设行为的名字。
fn preset(name: &str) -> Option<Behavior> {
    match name {
        "strip-cat" => Some(strip_cat()),
        "spider" => Some(spider()),
        "warrior" => Some(warrior()),
        _ => None,
    }
}

fn behavior_of(def: BehaviorDef) -> Result<Behavior, String> {
    let states = def
        .states
        .into_iter()
        .map(|state| StateDef {
            clip: state.clip,
            tag: state.tag,
            walk: state.walk.map(|walk| Walk {
                speed: walk.speed,
                axes: match walk.axes {
                    AxesFile::Horizontal => Axes::Horizontal,
                    AxesFile::Free => Axes::Free,
                },
            }),
            duration: state.duration,
        })
        .collect();
    let transitions = def
        .transitions
        .into_iter()
        .map(|transition| Transition {
            from: transition.from,
            to: transition.to,
            trigger: match transition.on {
                TriggerFile::Timeout => Trigger::Timeout,
                TriggerFile::Key => Trigger::Key,
                TriggerFile::Edge => Trigger::Edge,
                TriggerFile::Click => Trigger::Click,
                TriggerFile::Quiet(ms) => Trigger::Quiet(ms),
                TriggerFile::Near(near) => Trigger::Near {
                    range: near.range,
                    tag: near.tag,
                },
            },
            weight: transition.weight,
        })
        .collect();
    Ok(Behavior {
        states,
        transitions,
        start: def.start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACK: &str = r#"{
        "format": 1,
        "name": "测试",
        "clips": {
            "idle": {"ms": 200, "frames": [0, 1]},
            "walk": {"ms": 200, "frames": [2, 3]},
            "hit": {"ms": 100, "frames": [4]}
        },
        "actors": [
            {"count": 2, "layer": "above", "behavior": "warrior"},
            {"behavior": {
                "states": [
                    {"clip": "idle", "duration": [1000, 2000]},
                    {"clip": "walk", "walk": {"speed": 30, "axes": "horizontal"}, "duration": [2000, 3000]},
                    {"clip": "hit", "tag": "hurt", "duration": [300, 400]}
                ],
                "transitions": [
                    {"from": 0, "to": 1, "on": "timeout", "weight": 2},
                    {"from": 1, "to": 0, "on": "timeout"},
                    {"from": 0, "to": 2, "on": "click"},
                    {"from": 0, "to": 2, "on": {"near": {"range": 40, "tag": "attack"}}},
                    {"from": 2, "to": 0, "on": {"quiet": 1200}}
                ]
            }}
        ]
    }"#;

    #[test]
    fn a_full_pack_parses_with_presets_and_inline_behaviour() {
        let pack = parse(PACK).expect("合法的包");
        assert_eq!(pack.name, "测试");
        assert_eq!(pack.frames, ".");
        assert_eq!(pack.pack.max_frame(), Some(4));
        assert_eq!(pack.actors.len(), 2);
        assert_eq!(pack.actors[0].count, 2);
        assert_eq!(pack.actors[0].layer, Layer::Above);
        assert_eq!(pack.actors[1].count, 1);
        assert_eq!(pack.actors[1].layer, Layer::Below);
        let inline = &pack.actors[1].behavior;
        assert_eq!(inline.states[2].tag.as_deref(), Some("hurt"));
        assert!(matches!(
            inline.transitions[3].trigger,
            Trigger::Near { .. }
        ));
        assert!(matches!(
            inline.transitions[4].trigger,
            Trigger::Quiet(1200)
        ));
    }

    #[test]
    fn bad_packs_say_what_is_wrong() {
        assert!(parse("{").unwrap_err().contains("解析失败"));
        let wrong_format = PACK.replace("\"format\": 1", "\"format\": 2");
        assert!(parse(&wrong_format).unwrap_err().contains("格式版本"));
        let no_frames = PACK.replace("[4]", "[]");
        assert!(parse(&no_frames).unwrap_err().contains("没有帧"));
        let no_preset = PACK.replace("\"warrior\"", "\"dragon\"");
        assert!(parse(&no_preset).unwrap_err().contains("预设 dragon"));
        let missing_clip = PACK.replace("\"clip\": \"hit\"", "\"clip\": \"fly\"");
        assert!(parse(&missing_clip).unwrap_err().contains("片段 fly"));
        let too_many = PACK.replace("\"count\": 2", "\"count\": 999");
        assert!(parse(&too_many).unwrap_err().contains("数量"));
        let unknown_field = PACK.replace("\"name\": \"测试\"", "\"name\": \"测试\", \"speed\": 3");
        assert!(parse(&unknown_field).unwrap_err().contains("解析失败"));
    }
}
