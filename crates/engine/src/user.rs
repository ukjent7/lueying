//! 用户词库:个人学习的唯一存储,全局一份,不分应用。
//!
//! 学的东西有三类:读音词(学习键 + 词)、原样输入(用户实际敲的字母 + 词)与词序(前词 → 后词)。
//! 三者共用一个记忆模型 [`Memory`]:每用一次,记忆稳定度随之增长;不用时按幂律遗忘;条目永不删除。
//! 误选与跳过只减弱记忆,没有任何手动删除入口。
//!
//! 打字习惯(模糊组、快捷词抑制)不是词的记忆,单独放在 `user_typing`。
//!
//! 文件布局:`user_learn` 放学习与打分,`user_io` 放导出与导入。

use std::collections::{HashMap, HashSet};
#[cfg(any(test, feature = "sim-clock"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::user_typing::Typing;

const DAY_SECS: u64 = 86_400;

/// 遗忘曲线取 FSRS-4.5/5 公布的幂律形式 R(t, S) = (1 + F·t/S)^C,C = −0.5。
/// F 由「t = S 时保持率为 0.9」反解:F = 0.9^(1/C) − 1 = 0.9^(−2) − 1 = 19/81。
/// 稳定度 S 的单位是天,即保持率降到 0.9 所需的天数。
const FORGET_C: f64 = -0.5;
const FORGET_F: f64 = 19.0 / 81.0;
/// 第一次使用后的稳定度:一天后保持率约 0.9。之后每次在保持率 R 下使用,
/// 稳定度乘 (2 − R):刚用过就再用几乎不增长,久不用才再用时增长接近一倍。
const INITIAL_STABILITY: f64 = 1.0;

/// 层级达到这个值才抢首位:期望仍记得的使用次数达到两次,即「新词两三次才成为首选」。
pub(crate) const PROMOTE_LEVEL: f64 = 1.0;
/// 误选惩罚的遗忘门槛:误选记号按初始稳定度衰减,保持率跌破一半即解除。
const MISS_FLOOR: f64 = 0.5;
/// 「立刻删掉重打」的时间窗口(秒):上屏后这段时间内同键改选别的词,算误选。
pub(crate) const MISPICK_SECS: u64 = 60;

/// 模拟时钟(秒);0 表示用系统时钟。只在测试与 `sim-clock` 特性下存在,产品构建没有这个口子。
#[cfg(any(test, feature = "sim-clock"))]
static SIM_NOW: AtomicU64 = AtomicU64::new(0);

/// 固定学习用的当前时间(秒);传 0 恢复系统时钟。仅供评测与测试使用(需 `sim-clock` 特性)。
#[cfg(any(test, feature = "sim-clock"))]
pub fn set_sim_now(secs: u64) {
    SIM_NOW.store(secs, Ordering::Relaxed);
}

pub(crate) fn now() -> u64 {
    #[cfg(any(test, feature = "sim-clock"))]
    {
        let sim = SIM_NOW.load(Ordering::Relaxed);
        if sim != 0 {
            return sim;
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 当前天数。解码的前缀图按天失效:记忆随时间衰减,同一天内的变化忽略不计。
pub(crate) fn day() -> u64 {
    now() / DAY_SECS
}

/// 从 `last` 到 `now` 的天数。
fn elapsed_days(last: u64, now: u64) -> f64 {
    now.saturating_sub(last) as f64 / DAY_SECS as f64
}

/// 稳定度为 `stability` 天时,过了 `days` 天的保持率。
pub(crate) fn retention(stability: f64, days: f64) -> f64 {
    (1.0 + FORGET_F * days / stability).powf(FORGET_C)
}

/// 一条记忆:使用次数、稳定度与最近一次使用时刻。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Memory {
    pub uses: u32,
    pub stability: f64,
    pub last: u64,
}

impl Memory {
    /// 按当前时刻的有效层级:期望仍记得的使用次数,再减去首次使用(首次不算证据)。
    /// 刚用过三次时为 2;闲置后保持率下降,层级随之下降,直到归零。
    pub(crate) fn level(&self, now: u64) -> f64 {
        if self.uses == 0 {
            return 0.0;
        }
        let kept = retention(self.stability, elapsed_days(self.last, now));
        (f64::from(self.uses) * kept - 1.0).max(0.0)
    }

    /// 一次使用:先按当前保持率增长稳定度,再记下这次时刻。
    pub(crate) fn reinforce(&mut self, now: u64) {
        if self.uses == 0 {
            self.stability = INITIAL_STABILITY;
        } else {
            let kept = retention(self.stability, elapsed_days(self.last, now));
            self.stability *= 2.0 - kept;
        }
        self.uses = self.uses.saturating_add(1);
        self.last = now;
    }

    /// 负反馈:撤销一次使用(跳过或误选),稳定度与时刻保持不变。
    pub(crate) fn weaken(&mut self) {
        self.uses = self.uses.saturating_sub(1);
    }
}

#[derive(Clone)]
pub(crate) struct WordEntry {
    pub word: String,
    pub memory: Memory,
    /// 最近一次误选的时刻;`None` 表示没有误选记录。
    pub miss: Option<u64>,
}

#[derive(Default)]
pub struct UserDict {
    /// 学习键 → 词条。
    words: HashMap<String, Vec<WordEntry>>,
    /// 首字母变体(见 [`variants`]) → 学习键。查询按单元的首字母直接命中,不扫描。
    by_initials: HashMap<String, Vec<String>>,
    /// 各变体的音节前缀(以隔音符结尾)。判断「能否接更长的用户词」时查这里。
    prefixes: HashSet<String>,
    /// 词条总数。
    count: usize,
    /// 击键串(小写、无隔音符)→ 上屏过的词。只在击键串逐字相同时命中,不经读音。
    raw: HashMap<String, Vec<(String, Memory)>>,
    /// 前词 → 后词 → 记忆。只记相邻词对。
    transitions: HashMap<String, HashMap<String, Memory>>,
    /// 词 → 最完整的拼音学习键;下一词联想用它把已学词还原成可匹配的拼音。
    word_keys: HashMap<String, String>,
    /// 学习键 → 置顶词(用户手动置顶,不属于学习)。
    pinned: HashMap<String, String>,
    /// 打字习惯:模糊组与快捷词抑制。
    typing: Typing,
    /// 内容每次变化自增;解码器据此与时钟一起判断前缀图是否仍有效。
    rev: u64,
}

/// 学习键的首字母变体:每个音节取首字母;zh/ch/sh 另取两字母。
/// 查询时每个单元只取一种形式(见 `Lexicon` 的 `initials_of`),所以这里把键的每种形式都建好。
fn variants(key: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    for (index, part) in key.split('\'').enumerate() {
        let Some(first) = part.get(..1) else {
            return Vec::new();
        };
        let mut options = vec![first];
        if matches!(part.get(..2), Some("zh" | "ch" | "sh")) {
            options.push(&part[..2]);
        }
        out = out
            .iter()
            .flat_map(|head| {
                options.iter().map(move |option| {
                    if index == 0 {
                        option.to_string()
                    } else {
                        format!("{head}'{option}")
                    }
                })
            })
            .collect();
    }
    out.sort();
    out.dedup();
    out
}

#[path = "user_io.rs"]
mod user_io;
#[path = "user_learn.rs"]
mod user_learn;
