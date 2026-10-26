//! 用户词典:选词提频、自动造词与字级搭配学习,带指数衰减,支持导出/导入。

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

    /// 一个候选词序列在当前前词下的个人语序加分。
    pub fn transition_bonus(&self, previous: Option<&str>, words: &[String]) -> f64 {
        if self.transitions.is_empty() {
            return 0.0;
        }
        let current = now();
        let mut prev: Option<&str> = previous;
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
    /// 不能因为当前键不同就留下旧记录。
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
            self.rev = self.rev.wrapping_add(1);
        }
        removed > 0
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
    /// 组级/字级/词级学习用 `@group`、`@context`、`@transition` 行;按内容排序保证文件稳定。
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
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
    }
}
