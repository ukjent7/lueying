//! 拼音输入引擎:键状态机 + 组合编辑 + 候选管理。平台无关,上屏以事件表达。
//!
//! 默认行为对齐微软拼音习惯:空格上屏首选、数字选词、Enter 上屏原文、
//! Backspace 逐字母回删、Shift 切中英(组合中先上屏再切)、中文态标点转全角。
//!
//! 结构分三层: [`types`] 是宿主与模块共享的输入 DTO,[`module`] 是可选功能
//! 模块的契约与注册表,其余模块是默认全拼/简拼实现。高级能力不往核心路径里
//! 堆条件分支,而是作为模块由宿主组合进来。
//!
//! 文件布局:`state` 放会话、配置与候选选择,`input` 放按键状态机与上屏。

mod address;
mod emoji;
mod english;
mod input;
pub mod module;
mod punct;
mod query;
mod quick;
mod script;
mod segment;
mod state;
mod symbols;
pub mod types;
mod typo;
mod user;

use std::sync::{Arc, RwLock};

use lueying_dict::Dictionary;
pub use lueying_dict::Fuzzy;
use lueying_dict::gram::Gram;

pub use query::Candidate;
pub use quick::CivilTime;
pub use types::{Composition, Config, Event, Key, Mode};
pub use user::UserDict;

/// 功能模块作者常用的公开类型。
pub mod prelude {
    pub use crate::module::{
        CandidateContext, Capability, CommitContext, Manifest, Module, Modules,
    };
    pub use crate::{Candidate, Config, Engine, Event, Key, Mode};
}

/// 多个会话共享的用户词库;Server 让所有应用看到同一份学习结果。
pub type SharedUser = Arc<RwLock<UserDict>>;

/// 组合缓冲上限(字母与隔音符总数)。
/// 单次组合的原始输入上限。整句输入要能覆盖常见长句(约 26 个汉字);
/// 超过上限先落定当前组合,再用新按键起组,不能静默吞键。
pub const MAX_RAW: usize = 80;
/// 已上屏文本保留的字符数(重排与后续上下文模型使用)。
pub const CONTEXT_KEEP: usize = 128;

pub struct Engine {
    /// 词典跨会话共享(Server 进程里多个宿主会话共用一份)。
    dict: std::sync::Arc<Dictionary>,
    /// 可选的 rime `.gram` 语言模型;缺失或损坏时整句退化为纯词频。
    gram: Option<std::sync::Arc<Gram>>,
    /// 跨按键复用的搭配分缓存;键是 (前文,词,句末)。
    gram_cache: std::collections::HashMap<(String, String, bool), f64>,
    user: SharedUser,
    /// 私密输入范围里关闭学习;输入本身不受影响。
    learning: bool,
    cfg: Config,
    mode: Mode,
    /// 全角模式:英文态与中文态的空闲数字/空格/未映射标点按全角上屏。
    fullwidth: bool,
    comp: Option<Composition>,
    /// 当前组合里已经上屏过分段词(这时左邻文本不含这些词)。
    comp_committed: bool,
    /// 同一次组合里连续上屏的片段,凑成整串自动造词。
    phrase: Option<(String, String)>,
    cands: Vec<query::Candidate>,
    hl: usize,
    page: usize,
    dq_open: bool,
    sq_open: bool,
    /// 刚刚输入/上屏的是 ASCII 数字;中文模式下数字后的 `.` 保持半角。
    after_digit: bool,
    /// 最近上屏的文本,最多 [`CONTEXT_KEEP`] 字。焦点切换时清空。
    context: String,
    /// 分段上屏已在重算前写入上下文的提交数;`note_events` 据此跳过重复学习。
    context_handled: usize,
    /// 快捷候选上屏的条数;`note_events` 据此跳过字级搭配学习,但仍写入左邻文本。
    quick_handled: usize,
    /// 最近上屏的最后一个词;词级转移加分与学习使用。
    last_word: Option<String>,
    /// 最近上屏的倒数第二个词;三词窗口加分与学习使用。
    second_last_word: Option<String>,
    /// 宿主 exe 名(小写,不含路径);按应用学习使用,空表示未知宿主。
    host: String,
    /// 候选提交已在 `apply_consume` 学过词级转移的条数。
    transition_handled: usize,
    /// 宿主注入的本地时间;快捷输入按它生成日期/时间候选。
    now: Option<CivilTime>,
    /// 候选集合每次变化自增;异步重排据此丢弃过期结果。
    rev: u64,
    /// 可选功能模块;所有会话共享同一组注册表,默认空。
    modules: Arc<module::Modules>,
}

/// 文本末尾是不是数字;全角数字也按数字处理,小数点和 IP 地址不必切半角。
fn ends_with_digit(text: &str) -> bool {
    text.chars()
        .last()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, '０'..='９'))
}

/// 左邻文本里中文双引号、单引号是否还处在未闭合状态。
///
/// 只看最近 [`CONTEXT_KEEP`] 字;更早的开引号已经滚出上下文,这里无法判断,
/// 会按已闭合处理。正常的引号配对都在窗口内;跨过 128 字的长引文才会退化。
fn quote_state(text: &str) -> (bool, bool) {
    let mut dq_open = false;
    let mut sq_open = false;
    for c in text.chars() {
        match c {
            '“' => dq_open = true,
            '”' => dq_open = false,
            '‘' => sq_open = true,
            '’' => sq_open = false,
            _ => {}
        }
    }
    (dq_open, sq_open)
}
