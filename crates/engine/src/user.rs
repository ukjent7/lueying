//! 用户词典:选词提频与自动造词,带指数衰减,支持导出/导入。

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use lueying_dict::syllables::FUZZY_GROUPS;

/// 超过该间隔再选同一词,频次先减半再加一(近似近期使用优先)。
const DECAY_SECS: u64 = 7 * 24 * 3600;
/// 用户词条上限。超过后按频次/时间保留九成,防止长期使用后文件与查询无限膨胀。
const MAX_ENTRIES: usize = 20_000;
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

#[derive(Default)]
pub struct UserDict {
    map: HashMap<String, Vec<UserEntry>>,
    /// (音节数, 首字母) → 键。查询只扫同形用户词,用户词库到达上限后
    /// 也不会退化成每个音节组合线性扫全表;首字母的模糊等价由调用方展开。
    by_shape: HashMap<(usize, char), Vec<String>>,
    /// 词条总数,避免每次学习都扫全表。
    count: usize,
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

    /// 删除一条学习记录;返回是否命中。
    pub fn remove(&mut self, key: &str, word: &str) -> bool {
        let Some(entries) = self.map.get_mut(key) else {
            return false;
        };
        let Some(index) = entries.iter().position(|e| e.word == word) else {
            return false;
        };
        entries.remove(index);
        if entries.is_empty() {
            let key_shape = shape(key);
            self.map.remove(key);
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
        self.count = self.count.saturating_sub(1);
        self.rev = self.rev.wrapping_add(1);
        true
    }

    /// 该键下某词的用户频次(未学过为 0)。
    pub fn freq(&self, key: &str, word: &str) -> u32 {
        self.map
            .get(key)
            .and_then(|v| v.iter().find(|e| e.word == word))
            .map_or(0, |e| e.freq)
    }

    /// 键下的全部用户词条。
    pub fn entries(&self, key: &str) -> impl Iterator<Item = (&str, u32)> {
        self.map
            .get(key)
            .into_iter()
            .flat_map(|v| v.iter().map(|e| (e.word.as_str(), e.freq)))
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

    /// 导出为行文本(`键<TAB>词<TAB>频次<TAB>时间戳`),按键排序保证文件稳定。
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
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
    }
}
