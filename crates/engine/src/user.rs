//! 用户词典:选词提频、同键跳过降权、自动造词、字级搭配、词级/三词 n-gram
//! 与按应用偏好学习,带指数衰减,支持导出/导入。

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

impl UserDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// 内容修订号;只增不减。
    pub fn revision(&self) -> u64 {
        self.rev
    }

    /// 清空全部学习结果(设置窗的「清空用户词库」)。
    pub fn clear(&mut self) {
        self.map.clear();
        self.by_shape.clear();
        self.count = 0;
        self.context.clear();
        self.transitions.clear();
        self.trigrams.clear();
        self.apps.clear();
        self.rejects.clear();
        self.groups = [GroupStat::default(); GROUP_COUNT];
        self.rev = self.rev.wrapping_add(1);
    }

    /// 清空组级适应统计,保留词条。
    pub fn reset_groups(&mut self) {
        self.groups = [GroupStat::default(); GROUP_COUNT];
        self.rev = self.rev.wrapping_add(1);
    }

    /// 记录一次模糊组命中;每组按周衰减后加一。
    pub fn learn_groups(&mut self, mask: u8) {
        if mask == 0 {
            return;
        }
        let current = now();
        for (index, stat) in self.groups.iter_mut().enumerate() {
            if mask & (1 << index) == 0 {
                continue;
            }
            let weeks = current.saturating_sub(stat.ts) / GROUP_DECAY_SECS;
            stat.count = stat
                .count
                .checked_shr(weeks.min(31) as u32)
                .unwrap_or(0)
                .saturating_add(1);
            stat.ts = current;
        }
        self.rev = self.rev.wrapping_add(1);
    }

    /// 命中这些组时给候选的加分;低于模糊罚分,精确首位仍由 `trusted` 保护。
    pub fn group_bonus(&self, mask: u8) -> f64 {
        if mask == 0 {
            return 0.0;
        }
        let current = now();
        let mut bonus = 0.0;
        for (index, stat) in self.groups.iter().enumerate() {
            if mask & (1 << index) == 0 {
                continue;
            }
            let effective = stat.effective(current);
            // 前两次由词级学习负责;同一组连续选到第三次才开始整组上浮。
            if effective >= 3 {
                bonus += f64::from(effective.min(GROUP_BONUS_COUNT_CAP)) * GROUP_BONUS_STEP;
            }
        }
        bonus.min(GROUP_BONUS_MAX)
    }

    /// 设置页展示用:所有组及当前有效次数(已按周衰减)。
    pub fn group_stats(&self) -> Vec<(&'static str, u32)> {
        let current = now();
        FUZZY_GROUPS
            .iter()
            .copied()
            .zip(self.groups.iter().map(|stat| stat.effective(current)))
            .collect()
    }

    /// 记录一段已上屏文本与它左侧上下文的字级搭配。
    ///
    /// 一次上屏可能包含整句或多段词;句内相邻字、以及左邻紧挨着的汉字到
    /// 本次第一个汉字之间的衔接都会被记下来。只收汉字,标点和英文不参与。
    pub fn learn_context(&mut self, context: &str, text: &str) {
        let mut prev = tail_hanzi(context);
        let current = now();
        let mut changed = false;
        for ch in text.chars() {
            if !is_hanzi(ch) {
                prev = None;
                continue;
            }
            if let Some(left) = prev {
                let stat = self.context.entry((left, ch)).or_default();
                stat.count = if current.saturating_sub(stat.ts) > CONTEXT_DECAY_SECS {
                    stat.count / 2 + 1
                } else {
                    stat.count.saturating_add(1)
                };
                stat.ts = current;
                changed = true;
            }
            prev = Some(ch);
        }
        if !changed {
            return;
        }
        if self.context.len() > MAX_CONTEXT {
            self.prune_context();
        }
        self.rev = self.rev.wrapping_add(1);
    }

    /// 一个词在当前左邻上下文下的个人搭配加分。
    ///
    /// 词内相邻字也参与:用户常打的整句或词组会让它的内部搭配变强。加分
    /// 只做同分附近的个人化,上限低于一个普通词罚分。
    pub fn context_bonus(&self, context: &str, word: &str) -> f64 {
        if self.context.is_empty() {
            return 0.0;
        }
        let current = now();
        let mut prev = tail_hanzi(context);
        let mut bonus = 0.0;
        for ch in word.chars() {
            if !is_hanzi(ch) {
                prev = None;
                continue;
            }
            if let Some(left) = prev
                && let Some(stat) = self.context.get(&(left, ch))
            {
                let effective = stat.effective(current);
                if effective > 0 {
                    bonus += (1.0 + f64::from(effective)).ln() * CONTEXT_BONUS_STEP;
                }
            }
            prev = Some(ch);
        }
        bonus.min(CONTEXT_BONUS_MAX)
    }

    /// 记录一段已上屏词序列的相邻转移;`previous` 是上一段上屏的最后一个词。
    pub fn learn_transitions(&mut self, previous: Option<&str>, words: &[String]) {
        if words.is_empty() {
            return;
        }
        let current = now();
        let mut prev = previous.map(|text| text.to_owned());
        let mut changed = false;
        for word in words {
            if word.is_empty() {
                prev = None;
                continue;
            }
            if let Some(left) = prev.as_deref() {
                let stat = self
                    .transitions
                    .entry(left.to_string())
                    .or_default()
                    .entry(word.clone())
                    .or_default();
                stat.count = if current.saturating_sub(stat.ts) > TRANSITION_DECAY_SECS {
                    stat.count / 2 + 1
                } else {
                    stat.count.saturating_add(1)
                };
                stat.ts = current;
                changed = true;
            }
            prev = Some(word.clone());
        }
        if changed {
            if self.transition_count() > MAX_TRANSITIONS {
                self.prune_transitions();
            }
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// 记录一段词序列的三词窗口;`second_previous`/`previous` 是上屏前的最后两个词。
    pub fn learn_trigrams(
        &mut self,
        second_previous: Option<&str>,
        previous: Option<&str>,
        words: &[String],
    ) {
        if words.is_empty() {
            return;
        }
        let current = now();
        let mut prev2 = second_previous.map(|text| text.to_owned());
        let mut prev1 = previous.map(|text| text.to_owned());
        let mut changed = false;
        for word in words {
            if word.is_empty() {
                prev2 = None;
                prev1 = None;
                continue;
            }
            if let (Some(left), Some(mid)) = (prev2.as_deref(), prev1.as_deref()) {
                let stat = self
                    .trigrams
                    .entry(left.to_string())
                    .or_default()
                    .entry(mid.to_string())
                    .or_default()
                    .entry(word.clone())
                    .or_default();
                stat.count = if current.saturating_sub(stat.ts) > CONTEXT_DECAY_SECS {
                    stat.count / 2 + 1
                } else {
                    stat.count.saturating_add(1)
                };
                stat.ts = current;
                changed = true;
            }
            prev2 = prev1.take();
            prev1 = Some(word.clone());
        }
        if changed {
            if self.trigram_count() > MAX_TRIGRAMS {
                self.prune_trigrams();
            }
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// 一个候选词序列在当前前两词下的三词窗口加分。
    pub fn trigram_bonus<'a>(
        &self,
        second_previous: Option<&'a str>,
        previous: Option<&'a str>,
        words: &'a [String],
    ) -> f64 {
        if self.trigrams.is_empty() {
            return 0.0;
        }
        let current = now();
        let mut prev2 = second_previous;
        let mut prev1 = previous;
        let mut bonus = 0.0;
        for word in words {
            if word.is_empty() {
                prev2 = None;
                prev1 = None;
                continue;
            }
            if let (Some(left), Some(mid)) = (prev2, prev1)
                && let Some(stat) = self
                    .trigrams
                    .get(left)
                    .and_then(|next| next.get(mid))
                    .and_then(|next| next.get(word.as_str()))
            {
                let effective = stat.effective(current);
                if effective > 0 {
                    bonus += (1.0 + f64::from(effective)).ln() * TRIGRAM_BONUS_STEP;
                }
            }
            prev2 = prev1;
            prev1 = Some(word.as_str());
        }
        bonus.min(TRIGRAM_BONUS_MAX)
    }

    /// 记录一段已上屏词在宿主应用内的使用。只影响该应用下的排序,
    /// 不改全局词频;宿主名统一小写,空名(未知宿主)不学习。
    pub fn learn_app(&mut self, host: &str, words: &[String]) {
        let host = host.trim().to_ascii_lowercase();
        if host.is_empty() || words.iter().all(|word| word.is_empty()) {
            return;
        }
        let current = now();
        let app = self.apps.entry(host).or_default();
        let mut changed = false;
        for word in words {
            if word.is_empty() {
                continue;
            }
            let stat = app.entry(word.clone()).or_default();
            stat.count = if current.saturating_sub(stat.ts) > CONTEXT_DECAY_SECS {
                stat.count / 2 + 1
            } else {
                stat.count.saturating_add(1)
            };
            stat.ts = current;
            changed = true;
        }
        if changed {
            if self.app_count() > MAX_APP_WORDS {
                self.prune_apps();
            }
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// 一个候选词序列在指定应用内的个人偏好加分。`host` 需已是小写;
    /// 应用内常用词只做同分附近的个人化。
    pub fn app_bonus(&self, host: &str, words: &[String]) -> f64 {
        if host.is_empty() || self.apps.is_empty() {
            return 0.0;
        }
        let Some(app) = self.apps.get(host) else {
            return 0.0;
        };
        let current = now();
        let mut bonus = 0.0;
        for word in words {
            if word.is_empty() {
                continue;
            }
            if let Some(stat) = app.get(word.as_str()) {
                let effective = stat.effective(current);
                if effective > 0 {
                    bonus += (1.0 + f64::from(effective)).ln() * APP_BONUS_STEP;
                }
            }
        }
        bonus.min(APP_BONUS_MAX)
    }

    /// 一个候选词序列在当前前词下的个人语序加分。
    pub fn transition_bonus<'a>(&self, previous: Option<&'a str>, words: &'a [String]) -> f64 {
        if self.transitions.is_empty() {
            return 0.0;
        }
        let current = now();
        let mut prev: Option<&'a str> = previous;
        let mut bonus = 0.0;
        for word in words {
            if word.is_empty() {
                prev = None;
                continue;
            }
            if let Some(left) = prev
                && let Some(stat) = self
                    .transitions
                    .get(left)
                    .and_then(|next| next.get(word.as_str()))
            {
                let effective = stat.effective(current);
                if effective > 0 {
                    bonus += (1.0 + f64::from(effective)).ln() * TRANSITION_BONUS_STEP;
                }
            }
            prev = Some(word.as_str());
        }
        bonus.min(TRANSITION_BONUS_MAX)
    }

    fn transition_count(&self) -> usize {
        self.transitions.values().map(|next| next.len()).sum()
    }

    /// 保留最常见的前九成词对;一次性重建,摊到每次学习里的成本可忽略。
    fn prune_transitions(&mut self) {
        let keep = MAX_TRANSITIONS * 9 / 10;
        let mut all: Vec<(String, String, ContextStat)> = self
            .transitions
            .drain()
            .flat_map(|(left, next)| {
                next
                    .into_iter()
                    .map(move |(right, stat)| (left.clone(), right, stat))
            })
            .collect();
        all.sort_by(|a, b| {
            b.2.count
                .cmp(&a.2.count)
                .then(b.2.ts.cmp(&a.2.ts))
                .then(a.0.cmp(&b.0))
                .then(a.1.cmp(&b.1))
        });
        all.truncate(keep);
        for (left, right, stat) in all {
            self.transitions
                .entry(left)
                .or_default()
                .insert(right, stat);
        }
    }

    fn trigram_count(&self) -> usize {
        self.trigrams
            .values()
            .flat_map(|next| next.values())
            .map(|next| next.len())
            .sum()
    }

    /// 保留最常见的前九成三词窗口;一次性重建,摊到每次学习里的成本可忽略。
    fn prune_trigrams(&mut self) {
        let keep = MAX_TRIGRAMS * 9 / 10;
        let mut all: Vec<(String, String, String, ContextStat)> = Vec::new();
        for (left, mids) in self.trigrams.drain() {
            for (mid, following) in mids {
                for (right, stat) in following {
                    all.push((left.clone(), mid.clone(), right, stat));
                }
            }
        }
        all.sort_by(|a, b| {
            b.3.count
                .cmp(&a.3.count)
                .then(b.3.ts.cmp(&a.3.ts))
                .then(a.0.cmp(&b.0))
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
        });
        all.truncate(keep);
        for (left, mid, right, stat) in all {
            self.trigrams
                .entry(left)
                .or_default()
                .entry(mid)
                .or_default()
                .insert(right, stat);
        }
    }

    fn app_count(&self) -> usize {
        self.apps.values().map(|words| words.len()).sum()
    }

    /// 保留最常见的前九成应用词;一次性重建,摊到每次学习里的成本可忽略。
    fn prune_apps(&mut self) {
        let keep = MAX_APP_WORDS * 9 / 10;
        let mut all: Vec<(String, String, ContextStat)> = Vec::new();
        for (host, words) in self.apps.drain() {
            for (word, stat) in words {
                all.push((host.clone(), word, stat));
            }
        }
        all.sort_by(|a, b| {
            b.2.count
                .cmp(&a.2.count)
                .then(b.2.ts.cmp(&a.2.ts))
                .then(a.0.cmp(&b.0))
                .then(a.1.cmp(&b.1))
        });
        all.truncate(keep);
        for (host, word, stat) in all {
            self.apps.entry(host).or_default().insert(word, stat);
        }
    }

    /// 记录一次「在 key 下跳过了 word」。用户选择了同键下更靠后的候选,
    /// 说明排在前面的词不符合当前意图;下次同键时让它在同分附近后退。
    pub fn reject(&mut self, key: &str, word: &str) {
        if key.is_empty() || word.is_empty() {
            return;
        }
        let current = now();
        let existed = self
            .rejects
            .get(key)
            .is_some_and(|words| words.contains_key(word));
        let stat = self
            .rejects
            .entry(key.to_string())
            .or_default()
            .entry(word.to_string())
            .or_default();
        stat.count = if current.saturating_sub(stat.ts) > REJECT_DECAY_SECS {
            stat.count / 2 + 1
        } else {
            stat.count.saturating_add(1)
        };
        stat.ts = current;
        // 只有新增键集合才会逼近上限,已有记录更新不必扫全表。
        if !existed && self.reject_count() > MAX_REJECTS {
            self.prune_rejects();
        }
        self.rev = self.rev.wrapping_add(1);
    }

    /// 同键下的负向加分;次数越多越接近上限,但不超过一个普通词罚分。
    pub fn reject_penalty(&self, key: &str, word: &str) -> f64 {
        let Some(stat) = self.rejects.get(key).and_then(|words| words.get(word)) else {
            return 0.0;
        };
        let effective = stat.effective(now());
        if effective == 0 {
            return 0.0;
        }
        ((1.0 + f64::from(effective)).ln() * REJECT_BONUS_STEP).min(REJECT_BONUS_MAX)
    }

    /// 用户重新选中该词:清掉它在所有学习键下的跳过记录。
    /// 返回是否确实清掉了一条记录。
    pub fn forgive_word(&mut self, word: &str) -> bool {
        if self.rejects.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut empty = Vec::new();
        for (key, words) in &mut self.rejects {
            if words.remove(word).is_some() {
                changed = true;
                if words.is_empty() {
                    empty.push(key.clone());
                }
            }
        }
        for key in empty {
            self.rejects.remove(&key);
        }
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
        changed
    }

    /// 清掉某个学习键下的跳过记录;排名依据已经变了,旧负分不该继续生效。
    fn forgive(&mut self, key: &str, word: &str) {
        let empty = if let Some(words) = self.rejects.get_mut(key) {
            let _ = words.remove(word);
            words.is_empty()
        } else {
            false
        };
        if empty {
            self.rejects.remove(key);
        }
    }

    fn reject_count(&self) -> usize {
        self.rejects.values().map(|words| words.len()).sum()
    }

    /// 保留次数最多的前九成记录;一次性重建,摊到每次学习里的成本可忽略。
    fn prune_rejects(&mut self) {
        let keep = MAX_REJECTS * 9 / 10;
        let mut all: Vec<(String, String, RejectStat)> = self
            .rejects
            .drain()
            .flat_map(|(key, words)| {
                words
                    .into_iter()
                    .map(move |(word, stat)| (key.clone(), word, stat))
            })
            .collect();
        all.sort_by(|a, b| {
            b.2.count
                .cmp(&a.2.count)
                .then(b.2.ts.cmp(&a.2.ts))
                .then(a.0.cmp(&b.0))
                .then(a.1.cmp(&b.1))
        });
        all.truncate(keep);
        for (key, word, stat) in all {
            self.rejects.entry(key).or_default().insert(word, stat);
        }
    }

    /// 保留最常见的前九成搭配;一次性重建,摊到每次学习里的成本可忽略。
    fn prune_context(&mut self) {
        let keep = MAX_CONTEXT * 9 / 10;
        let mut all: Vec<((char, char), ContextStat)> = self.context.drain().collect();
        all.sort_by(|a, b| {
            b.1.count
                .cmp(&a.1.count)
                .then(b.1.ts.cmp(&a.1.ts))
                .then(a.0.cmp(&b.0))
        });
        all.truncate(keep);
        self.context.extend(all);
    }

    /// 记录一次「在 key 下选了 word」。
    pub fn learn(&mut self, key: &str, word: &str) {
        let ts = now();
        let new_key = !self.map.contains_key(key);
        let entries = self.map.entry(key.to_string()).or_default();
        match entries.iter_mut().find(|e| e.word == word) {
            Some(e) => {
                e.freq = if ts.saturating_sub(e.ts) > DECAY_SECS {
                    e.freq / 2 + 1
                } else {
                    e.freq.saturating_add(1)
                };
                e.ts = ts;
            }
            None => {
                entries.push(UserEntry {
                    word: word.to_string(),
                    freq: 1,
                    ts,
                });
                self.count += 1;
            }
        }
        if new_key {
            self.by_shape
                .entry(shape(key))
                .or_default()
                .push(key.to_string());
        }
        // 刚选过它,同键下的旧负分已经过时。
        self.forgive(key, word);
        if self.count > MAX_ENTRIES {
            self.prune();
        }
        self.rev = self.rev.wrapping_add(1);
    }

    /// 保留高频、近期的九成词条;一次性重建,摊到每次学习里的成本可忽略。
    fn prune(&mut self) {
        let keep = MAX_ENTRIES * 9 / 10;
        let mut all: Vec<(String, UserEntry)> = self
            .map
            .drain()
            .flat_map(|(key, entries)| entries.into_iter().map(move |e| (key.clone(), e)))
            .collect();
        all.sort_by(|a, b| {
            b.1.freq
                .cmp(&a.1.freq)
                .then(b.1.ts.cmp(&a.1.ts))
                .then(a.0.cmp(&b.0))
                .then(a.1.word.cmp(&b.1.word))
        });
        all.truncate(keep);
        self.by_shape.clear();
        for (key, entry) in all {
            self.by_shape
                .entry(shape(&key))
                .or_default()
                .push(key.clone());
            self.map.entry(key).or_default().push(entry);
        }
        // 同一个键可能有多条词,上面按词条遍历会重复加入形状索引。
        for keys in self.by_shape.values_mut() {
            keys.sort();
            keys.dedup();
        }
        self.count = self.map.values().map(Vec::len).sum();
    }

    /// 删除该词在所有学习键下的记录。用户可能用全拼学过、现在用简拼忘掉,
    /// 不能因为当前键不同就留下旧记录。返回是否删掉了学习记录
    /// (词条或跳过负分)。
    pub fn remove_word(&mut self, word: &str) -> bool {
        let mut removed = 0usize;
        let mut empty_keys = Vec::new();
        self.map.retain(|key, entries| {
            let before = entries.len();
            entries.retain(|e| e.word.as_str() != word);
            removed += before - entries.len();
            if entries.is_empty() {
                empty_keys.push(key.clone());
                false
            } else {
                true
            }
        });
        self.count = self.count.saturating_sub(removed);
        for key in &empty_keys {
            let key_shape = shape(key);
            let empty = if let Some(keys) = self.by_shape.get_mut(&key_shape) {
                keys.retain(|item| item != key);
                keys.is_empty()
            } else {
                false
            };
            if empty {
                self.by_shape.remove(&key_shape);
            }
        }
        if removed > 0 {
            self.forget_transitions(word);
            self.forget_trigrams(word);
            self.rev = self.rev.wrapping_add(1);
        }
        // 负分和应用偏好也是学习记录;Shift+Delete 要能一起忘掉。
        let forgot = self.forgive_word(word);
        let forgot_apps = self.forget_apps(word);
        removed > 0 || forgot || forgot_apps
    }

    /// 删掉一个词参与的相邻词对,避免 Shift+Delete 后旧语序还影响排序。
    fn forget_transitions(&mut self, word: &str) {
        self.transitions.remove(word);
        let mut emptied = Vec::new();
        for (left, next) in &mut self.transitions {
            next.remove(word);
            if next.is_empty() {
                emptied.push(left.clone());
            }
        }
        for left in emptied {
            self.transitions.remove(&left);
        }
    }

    /// 删掉一个词参与的任意位置三词窗口。
    fn forget_trigrams(&mut self, word: &str) {
        self.trigrams.retain(|left, mids| {
            if left.as_str() == word {
                return false;
            }
            mids.retain(|mid, next| {
                if mid.as_str() == word {
                    return false;
                }
                next.remove(word);
                !next.is_empty()
            });
            !mids.is_empty()
        });
    }

    /// 删掉一个词在所有应用里的偏好记录。返回是否确实删掉了记录。
    fn forget_apps(&mut self, word: &str) -> bool {
        let mut changed = false;
        self.apps.retain(|_, words| {
            if words.remove(word).is_some() {
                changed = true;
            }
            !words.is_empty()
        });
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
        changed
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 只遍历音节数与首字母可能匹配的用户键。首字母模糊等价由调用方展开;
    /// 同一音节数、同一首字母是逐音节匹配的必要条件,不会漏词。
    pub fn for_each_matching(
        &self,
        shapes: &[(usize, char)],
        mut f: impl FnMut(&str, &str, u32),
    ) {
        for shape in shapes {
            let Some(keys) = self.by_shape.get(shape) else {
                continue;
            };
            for key in keys {
                let Some(entries) = self.map.get(key) else {
                    continue;
                };
                for entry in entries {
                    f(key, &entry.word, entry.freq);
                }
            }
        }
    }

    /// 导出为行文本:词条 `键<TAB>词<TAB>频次<TAB>时间戳`,
    /// 组级/字级/词级/三词/应用/跳过学习用 `@group`、`@context`、`@transition`、
    /// `@trigram`、`@app`、`@reject` 行;按内容排序保证文件稳定。
    pub fn export(&self) -> String {
        let mut keys: Vec<_> = self.map.iter().collect();
        keys.sort_by(|a, b| a.0.cmp(b.0));
        let mut out = String::new();
        for (k, v) in keys {
            let mut words = v.iter().collect::<Vec<_>>();
            words.sort_by(|a, b| a.word.cmp(&b.word));
            for e in words {
                out.push_str(&format!("{k}\t{}\t{}\t{}\n", e.word, e.freq, e.ts));
            }
        }
        for (name, stat) in FUZZY_GROUPS.iter().zip(self.groups.iter()) {
            if stat.count > 0 {
                out.push_str(&format!("@group\t{name}\t{}:{}\n", stat.count, stat.ts));
            }
        }
        let mut context: Vec<(char, char)> = self.context.keys().copied().collect();
        context.sort();
        for (left, right) in context {
            let Some(stat) = self.context.get(&(left, right)) else {
                continue;
            };
            if stat.count > 0 {
                out.push_str(&format!(
                    "@context\t{left}\t{right}\t{}\t{}\n",
                    stat.count, stat.ts
                ));
            }
        }
        let mut previous: Vec<&String> = self.transitions.keys().collect();
        previous.sort();
        for left in previous {
            let Some(next) = self.transitions.get(left) else {
                continue;
            };
            let mut following: Vec<&String> = next.keys().collect();
            following.sort();
            for right in following {
                let Some(stat) = next.get(right) else {
                    continue;
                };
                if stat.count > 0 {
                    out.push_str(&format!(
                        "@transition\t{left}\t{right}\t{}\t{}\n",
                        stat.count, stat.ts
                    ));
                }
            }
        }
        let mut first: Vec<&String> = self.trigrams.keys().collect();
        first.sort();
        for left in first {
            let Some(mids) = self.trigrams.get(left) else {
                continue;
            };
            let mut second: Vec<&String> = mids.keys().collect();
            second.sort();
            for mid in second {
                let Some(next) = mids.get(mid) else {
                    continue;
                };
                let mut following: Vec<&String> = next.keys().collect();
                following.sort();
                for right in following {
                    let Some(stat) = next.get(right) else {
                        continue;
                    };
                    if stat.count > 0 {
                        out.push_str(&format!(
                            "@trigram\t{left}\t{mid}\t{right}\t{}\t{}\n",
                            stat.count, stat.ts
                        ));
                    }
                }
            }
        }
        let mut apps: Vec<&String> = self.apps.keys().collect();
        apps.sort();
        for host in apps {
            let Some(words) = self.apps.get(host) else {
                continue;
            };
            let mut used: Vec<&String> = words.keys().collect();
            used.sort();
            for word in used {
                let Some(stat) = words.get(word) else {
                    continue;
                };
                if stat.count > 0 {
                    out.push_str(&format!(
                        "@app\t{host}\t{word}\t{}\t{}\n",
                        stat.count, stat.ts
                    ));
                }
            }
        }
        let mut rejected: Vec<&String> = self.rejects.keys().collect();
        rejected.sort();
        for key in rejected {
            let Some(words) = self.rejects.get(key) else {
                continue;
            };
            let mut keys: Vec<&String> = words.keys().collect();
            keys.sort();
            for word in keys {
                let Some(stat) = words.get(word) else {
                    continue;
                };
                if stat.count > 0 {
                    out.push_str(&format!(
                        "@reject\t{key}\t{word}\t{}\t{}\n",
                        stat.count, stat.ts
                    ));
                }
            }
        }
        out
    }

    /// 从 [`Self::export`] 的输出导入,坏行跳过,同键同词合并。
    pub fn import(&mut self, text: &str) {
        let mut changed = false;
        for line in text.lines() {
            let mut parts = line.split('\t');
            let Some(k) = parts.next() else {
                continue;
            };
            if k == "@group" {
                let (Some(name), Some(value), None) = (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                let Some((count, ts)) = value.split_once(':') else {
                    continue;
                };
                let (Ok(count), Ok(ts)) = (count.parse::<u32>(), ts.parse::<u64>()) else {
                    continue;
                };
                let Some(index) = FUZZY_GROUPS.iter().position(|group| *group == name) else {
                    continue;
                };
                let stat = &mut self.groups[index];
                stat.count = stat.count.max(count);
                stat.ts = stat.ts.max(ts);
                changed = true;
                continue;
            }
            if k == "@context" {
                let (Some(left), Some(right), Some(count), Some(ts), None) = (
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                ) else {
                    continue;
                };
                let (Some(left), Some(right)) = (single_char(left), single_char(right)) else {
                    continue;
                };
                let (Ok(count), Ok(ts)) = (count.parse::<u32>(), ts.parse::<u64>()) else {
                    continue;
                };
                if !is_hanzi(left) || !is_hanzi(right) || count == 0 {
                    continue;
                }
                let stat = self.context.entry((left, right)).or_default();
                stat.count = stat.count.max(count);
                stat.ts = stat.ts.max(ts);
                changed = true;
                continue;
            }
            if k == "@transition" {
                let (Some(left), Some(right), Some(count), Some(ts), None) = (
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                ) else {
                    continue;
                };
                let (Ok(count), Ok(ts)) = (count.parse::<u32>(), ts.parse::<u64>()) else {
                    continue;
                };
                if left.is_empty() || right.is_empty() || count == 0 {
                    continue;
                }
                let stat = self
                    .transitions
                    .entry(left.to_string())
                    .or_default()
                    .entry(right.to_string())
                    .or_default();
                stat.count = stat.count.max(count);
                stat.ts = stat.ts.max(ts);
                changed = true;
                continue;
            }
            if k == "@trigram" {
                let (Some(left), Some(mid), Some(right), Some(count), Some(ts), None) = (
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                ) else {
                    continue;
                };
                let (Ok(count), Ok(ts)) = (count.parse::<u32>(), ts.parse::<u64>()) else {
                    continue;
                };
                if left.is_empty() || mid.is_empty() || right.is_empty() || count == 0 {
                    continue;
                }
                let stat = self
                    .trigrams
                    .entry(left.to_string())
                    .or_default()
                    .entry(mid.to_string())
                    .or_default()
                    .entry(right.to_string())
                    .or_default();
                stat.count = stat.count.max(count);
                stat.ts = stat.ts.max(ts);
                changed = true;
                continue;
            }
            if k == "@app" {
                let (Some(host), Some(word), Some(count), Some(ts), None) = (
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                ) else {
                    continue;
                };
                let host = host.trim().to_ascii_lowercase();
                let (Ok(count), Ok(ts)) = (count.parse::<u32>(), ts.parse::<u64>()) else {
                    continue;
                };
                if host.is_empty() || word.is_empty() || count == 0 {
                    continue;
                }
                let stat = self
                    .apps
                    .entry(host)
                    .or_default()
                    .entry(word.to_string())
                    .or_default();
                stat.count = stat.count.max(count);
                stat.ts = stat.ts.max(ts);
                changed = true;
                continue;
            }
            if k == "@reject" {
                let (Some(key), Some(word), Some(count), Some(ts), None) = (
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                ) else {
                    continue;
                };
                let (Ok(count), Ok(ts)) = (count.parse::<u32>(), ts.parse::<u64>()) else {
                    continue;
                };
                if key.is_empty() || word.is_empty() || count == 0 {
                    continue;
                }
                let stat = self
                    .rejects
                    .entry(key.to_string())
                    .or_default()
                    .entry(word.to_string())
                    .or_default();
                stat.count = stat.count.max(count);
                stat.ts = stat.ts.max(ts);
                changed = true;
                continue;
            }
            let (Some(w), Some(f), Some(t), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if let (Ok(freq), Ok(ts)) = (f.parse::<u32>(), t.parse::<u64>())
                && !k.is_empty()
                && !w.is_empty()
            {
                let new_key = !self.map.contains_key(k);
                let entries = self.map.entry(k.to_string()).or_default();
                match entries.iter_mut().find(|e| e.word == w) {
                    Some(e) => {
                        e.freq = e.freq.max(freq);
                        e.ts = e.ts.max(ts);
                    }
                    None => {
                        entries.push(UserEntry {
                            word: w.to_string(),
                            freq,
                            ts,
                        });
                        self.count += 1;
                    }
                }
                if new_key {
                    self.by_shape
                        .entry(shape(k))
                        .or_default()
                        .push(k.to_string());
                }
                changed = true;
            }
        }
        if self.count > MAX_ENTRIES {
            self.prune();
        }
        if self.context.len() > MAX_CONTEXT {
            self.prune_context();
        }
        if self.transition_count() > MAX_TRANSITIONS {
            self.prune_transitions();
        }
        if self.trigram_count() > MAX_TRIGRAMS {
            self.prune_trigrams();
        }
        if self.app_count() > MAX_APP_WORDS {
            self.prune_apps();
        }
        if self.reject_count() > MAX_REJECTS {
            self.prune_rejects();
        }
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
    }
}
