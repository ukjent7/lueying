//! 用户词典:选词提频、同键跳过降权、自动造词、字级搭配、词级/三词 n-gram
//! 与按应用偏好学习,带指数衰减,支持导出/导入。
//!
//! 文件布局:`user_learn` 放学习与打分,`user_io` 放删除、导出与导入。

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use lueying_dict::syllables::FUZZY_GROUPS;

/// 超过该间隔再选同一词,频次先减半再加一(近似近期使用优先)。
const DECAY_SECS: u64 = 7 * 24 * 3600;
/// 用户词条上限。超过后按频次/时间保留九成,防止长期使用后文件与查询无限膨胀。
const MAX_ENTRIES: usize = 20_000;
/// 字级上下文搭配上限。只保留最常见的前后字对,避免长期使用后文件无界增长。
const MAX_CONTEXT: usize = 50_000;
/// 上下文搭配的衰减周期与加分权重。它只做同分附近的个人化，不能压过词典证据。
const CONTEXT_DECAY_SECS: u64 = 7 * 24 * 3600;
const CONTEXT_BONUS_STEP: f64 = 0.25;
const CONTEXT_BONUS_MAX: f64 = 1.5;
/// 词级转移上限与加分。词比单字更有区分度，但仍只做同分附近的个人语序。
const MAX_TRANSITIONS: usize = 20_000;
const TRANSITION_DECAY_SECS: u64 = 7 * 24 * 3600;
const TRANSITION_BONUS_STEP: f64 = 0.2;
const TRANSITION_BONUS_MAX: f64 = 1.2;
/// 三词窗口上限与加分。三词比词对少见,只给反复出现的固定说法一点顺序分;
/// 衰减复用字级搭配的每周减半。
const MAX_TRIGRAMS: usize = 20_000;
const TRIGRAM_BONUS_STEP: f64 = 0.12;
const TRIGRAM_BONUS_MAX: f64 = 0.9;
/// 按应用的词偏好上限与加分。应用内常用词只做同分附近的个人化,
/// 全局词频仍是主依据。
const MAX_APP_WORDS: usize = 20_000;
const APP_BONUS_STEP: f64 = 0.15;
const APP_BONUS_MAX: f64 = 0.9;
/// 同键下被跳过的候选。用户每次选后面的词,前面被越过的候选记一次小负分;
/// 再选中它时清除。负分只够翻动同分附近,不会把常用词压到长尾后面。
const MAX_REJECTS: usize = 20_000;
const REJECT_DECAY_SECS: u64 = 7 * 24 * 3600;
const REJECT_BONUS_STEP: f64 = 0.28;
const REJECT_BONUS_MAX: f64 = 1.4;
/// 组级适应按周减半;用户改口音或换输入习惯后一周内就会自然回退,
/// 不会因为历史上选过几次就让所有组一起顶到上限、失去区分度。
const GROUP_DECAY_SECS: u64 = 7 * 24 * 3600;
/// 每组每选一次给模糊候选加的分,以及单组/总计的上限。
const GROUP_BONUS_STEP: f64 = 0.12;
const GROUP_BONUS_COUNT_CAP: u32 = 5;
const GROUP_BONUS_MAX: f64 = 0.6;
const GROUP_COUNT: usize = FUZZY_GROUPS.len();

#[derive(Clone, Copy, Default)]
struct GroupStat {
    count: u32,
    ts: u64,
}

impl GroupStat {
    /// 按完整周数减半后的有效次数。
    fn effective(self, current: u64) -> u32 {
        let weeks = current.saturating_sub(self.ts) / GROUP_DECAY_SECS;
        self.count.checked_shr(weeks.min(31) as u32).unwrap_or(0)
    }
}

#[derive(Clone)]
struct UserEntry {
    word: String,
    freq: u32,
    ts: u64,
}

#[derive(Clone, Copy, Default)]
struct ContextStat {
    count: u32,
    ts: u64,
}

impl ContextStat {
    fn effective(self, current: u64) -> u32 {
        let weeks = current.saturating_sub(self.ts) / CONTEXT_DECAY_SECS;
        self.count.checked_shr(weeks.min(31) as u32).unwrap_or(0)
    }
}

#[derive(Clone, Copy, Default)]
struct RejectStat {
    count: u32,
    ts: u64,
}

impl RejectStat {
    fn effective(self, current: u64) -> u32 {
        let weeks = current.saturating_sub(self.ts) / REJECT_DECAY_SECS;
        self.count.checked_shr(weeks.min(31) as u32).unwrap_or(0)
    }
}

#[derive(Default)]
pub struct UserDict {
    map: HashMap<String, Vec<UserEntry>>,
    /// (音节数, 首字母) → 键。查询只扫同形用户词,用户词库到达上限后
    /// 也不会退化成每个音节组合线性扫全表;首字母的模糊等价由调用方展开。
    by_shape: HashMap<(usize, char), Vec<String>>,
    /// 词条总数,避免每次学习都扫全表。
    count: usize,
    /// 已上屏汉字的前后搭配。用户在不同词之间形成的固定说法会随使用增强,
    /// 只用于候选整句重排,不单独产生候选。
    context: HashMap<(char, char), ContextStat>,
    /// 前词 → 后词 → 次数。只记相邻词对,区分度高于字对。
    transitions: HashMap<String, HashMap<String, ContextStat>>,
    /// 前两词 → 后词 → 次数。三词窗口约束常见句式,权重低于词对。
    trigrams: HashMap<String, HashMap<String, HashMap<String, ContextStat>>>,
    /// 宿主 exe(小写) → 词 → 次数。应用内常用词只加小分,不改全局词频。
    apps: HashMap<String, HashMap<String, ContextStat>>,
    /// 学习键 → 被跳过的词 → 次数。用户在同键下越过它选了别的词;
    /// 只小幅降权,并让精确首位保护让位。
    rejects: HashMap<String, HashMap<String, RejectStat>>,
    /// 各模糊组的连续选择次数,与词条一起持久化。
    groups: [GroupStat; GROUP_COUNT],
    /// 内容每次变化自增;持久化线程据此决定是否写盘。
    rev: u64,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn shape(key: &str) -> (usize, char) {
    (key.split('\'').count(), key.chars().next().unwrap_or('\0'))
}

/// 上下文只收汉字;标点、数字和英文不进入搭配统计。
fn is_hanzi(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        | 0x20000..=0x2A6DF | 0x2A700..=0x2EBEF | 0x30000..=0x323AF)
}

/// 只把紧邻的汉字算作搭配:末尾是标点、空格、数字或英文时,后面的字
/// 不与更早的汉字构成搭配。
fn tail_hanzi(text: &str) -> Option<char> {
    text.chars().next_back().filter(|c| is_hanzi(*c))
}

fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

#[path = "user_io.rs"]
mod user_io;
#[path = "user_learn.rs"]
mod user_learn;
