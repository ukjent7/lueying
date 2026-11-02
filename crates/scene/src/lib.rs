//! 声明式场景运行时:用户用 `scene.toml` 描述会对输入做出反应的界面
//! 元素——光晕、精灵、粒子、补间——运行时把它们确定性地产出为图元
//! 清单,窗口层接上 D2D 画出来。
//!
//! 三块契约,一次定死:
//! - [`model`][]:场景文件的语法与校验,中文报错带文件、行与节点。
//! - [`expr`][]:有界表达式,算术、比较与一组纯函数,无循环无递归。
//! - [`runtime`][]:确定性时间线,信号进、图元出,随机走种子。
//!
//! 护栏都在:节点与规则数量上限、每帧表达式预算、粒子池与事件队列
//! 上限。场景是装饰,永远不能拖垮输入。

pub mod expr;
pub mod model;
pub mod replay;
pub mod runtime;

pub use model::{Fill, Layer, PaletteName, Scene, Shape, parse};
pub use replay::{Script, replay};
pub use runtime::{Body, EffectOut, EventName, Frame, Inputs, Item, Particle, Rect, Runtime};

/// 场景文件的 JSON Schema;`lueying scene schema` 原样输出,编辑器拿它
/// 做自动补全与即时报错。
pub const SCHEMA: &str = include_str!("../scene.schema.json");
