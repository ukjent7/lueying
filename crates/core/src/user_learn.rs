//! 用户词库的学习与打分:词记忆(选中、误选、跳过)、词序记忆与联想。
//!
//! 类型与常量在 `user.rs`,这里是子模块,可以直接访问私有字段。

use super::*;

/// 词序加分:每个有效层级的对数权重与上限,沿用旧设计的分数尺度。
const TRANSITION_BONUS_STEP: f64 = 0.2;
const TRANSITION_BONUS_MAX: f64 = 1.2;

fn multi_char(word: &str) -> bool {
    word.chars().nth(1).is_some()
}

impl UserDict {
    pub fn new() -> Self {
        Self::default()
    }

    /// 内容修订号;只增不减。
    pub fn revision(&self) -> u64 {
        self.rev
    }

    fn entry(&self, key: &str, word: &str) -> Option<&WordEntry> {
        self.words.get(key)?.iter().find(|e| e.word == word)
    }

    fn entry_mut(&mut self, key: &str, word: &str) -> Option<&mut WordEntry> {
        self.words.get_mut(key)?.iter_mut().find(|e| e.word == word)
    }

    /// 同键下 `word` 的有效层级(见 `Memory::level`);没有记录为 0。
    pub fn level(&self, key: &str, word: &str) -> f64 {
        self.entry(key, word).map_or(0.0, |e| e.memory.level(now()))
    }

    /// 同键下的词是否已达到首位门槛(与词库首位保护同一标准)。
    pub fn promoted(&self, key: &str, word: &str) -> bool {
        self.level(key, word) >= PROMOTE_LEVEL
    }

    /// 同键下刚被误选过、记忆仍够清楚:这个词不许占首位。
    pub fn penalized(&self, key: &str, word: &str) -> bool {
        let now = now();
        self.entry(key, word).is_some_and(|e| {
            e.miss
                .is_some_and(|at| retention(INITIAL_STABILITY, elapsed_days(at, now)) >= MISS_FLOOR)
        })
    }

    /// 记一次「在 key 下选了 word」。词典里有没有这个词一视同仁:每次使用都是证据。
    pub fn learn(&mut self, key: &str, word: &str) {
        if key.is_empty() || word.is_empty() {
            return;
        }
        let now = now();
        let new_key = !self.words.contains_key(key);
        let entries = self.words.entry(key.to_string()).or_default();
        match entries.iter_mut().find(|e| e.word == word) {
            Some(entry) => {
                entry.memory.reinforce(now);
                // 重新选中它,之前的误选记号就不再适用。
                entry.miss = None;
            }
            None => {
                let mut memory = Memory::default();
                memory.reinforce(now);
                entries.push(WordEntry {
                    word: word.to_string(),
                    memory,
                    miss: None,
                });
                self.count += 1;
            }
        }
        if new_key {
            self.index_key(key);
        }
        // 同一个词可能用简拼和全拼各学过一次;联想取更完整的那条键。
        if self
            .word_keys
            .get(word)
            .is_none_or(|old| old.len() < key.len())
        {
            self.word_keys.insert(word.to_string(), key.to_string());
        }
        self.rev = self.rev.wrapping_add(1);
    }

    /// 误选:上屏后立刻删掉重打并改选了别的词。撤销这次选择的强化,并记下误选时刻。
    pub fn mispick(&mut self, key: &str, word: &str) {
        let now = now();
        if let Some(entry) = self.entry_mut(key, word) {
            entry.memory.weaken();
            entry.miss = Some(now);
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// 跳过:同键下用户选了排在它后面的候选,它减弱一次。
    pub fn skip(&mut self, key: &str, word: &str) {
        if let Some(entry) = self.entry_mut(key, word) {
            entry.memory.weaken();
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// 原样输入记忆:用户敲的击键串 `typed`(小写、无隔音符)上屏了 `word`。同一击键串下改选了别的词,
    /// 之前学过的词被越过一次,减弱一次。
    pub fn learn_raw(&mut self, typed: &str, word: &str) {
        if typed.is_empty() || word.is_empty() {
            return;
        }
        let now = now();
        let entries = self.raw.entry(typed.to_string()).or_default();
        for (_, memory) in entries.iter_mut().filter(|(text, _)| text != word) {
            memory.weaken();
        }
        match entries.iter_mut().find(|(text, _)| text == word) {
            Some((_, memory)) => memory.reinforce(now),
            None => {
                let mut memory = Memory::default();
                memory.reinforce(now);
                entries.push((word.to_string(), memory));
            }
        }
        self.rev = self.rev.wrapping_add(1);
    }

    /// 击键串 `typed` 上屏过的词:(词, 有效层级, 使用次数)。
    pub(crate) fn raw_words(&self, typed: &str) -> Vec<(&str, f64, u32)> {
        let now = now();
        self.raw.get(typed).map_or_else(Vec::new, |entries| {
            entries
                .iter()
                .map(|(text, memory)| (text.as_str(), memory.level(now), memory.uses))
                .collect()
        })
    }

    /// 记录一段已上屏词序的相邻转移;`previous` 是上一段上屏的最后一个词。
    pub fn learn_transitions(&mut self, previous: Option<&str>, words: &[String]) {
        let now = now();
        let mut prev = previous.map(str::to_owned);
        let mut changed = false;
        for word in words {
            if word.is_empty() {
                prev = None;
                continue;
            }
            if let Some(left) = prev.as_deref() {
                self.transitions
                    .entry(left.to_string())
                    .or_default()
                    .entry(word.clone())
                    .or_default()
                    .reinforce(now);
                changed = true;
            }
            prev = Some(word.clone());
        }
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
    }

    /// 一个候选词序列在当前前词下的词序加分。
    pub fn transition_bonus(&self, previous: Option<&str>, words: &[String]) -> f64 {
        if self.transitions.is_empty() {
            return 0.0;
        }
        let now = now();
        let mut prev = previous;
        let mut bonus = 0.0;
        for word in words {
            if word.is_empty() {
                prev = None;
                continue;
            }
            // 只看多字词:单字的前后搭配是字级噪声,不能靠词序把切分拉偏。
            if let Some(left) = prev
                && multi_char(left)
                && multi_char(word)
                && let Some(memory) = self
                    .transitions
                    .get(left)
                    .and_then(|next| next.get(word.as_str()))
            {
                let level = memory.level(now);
                if level > 0.0 {
                    bonus += (1.0 + level).ln() * TRANSITION_BONUS_STEP;
                }
            }
            prev = Some(word.as_str());
        }
        bonus.min(TRANSITION_BONUS_MAX)
    }

    /// 上一词之后用户接过的词,按有效层级从高到低。下一词联想只用本机形成的词序。
    pub fn predict_next(&self, previous: Option<&str>, limit: usize) -> Vec<String> {
        let (Some(previous), now) = (previous, now()) else {
            return Vec::new();
        };
        let Some(next) = self.transitions.get(previous) else {
            return Vec::new();
        };
        let mut out: Vec<(f64, &str)> = next
            .iter()
            .map(|(word, memory)| (memory.level(now), word.as_str()))
            .filter(|(level, _)| *level > 0.0)
            .collect();
        out.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        out.into_iter()
            .take(limit)
            .map(|(_, word)| word.to_string())
            .collect()
    }

    /// 词对应的最完整拼音键;联想据此判断用户当前输入是否在打这个词。
    pub fn key_for_word(&self, word: &str) -> Option<&str> {
        self.word_keys.get(word).map(String::as_str)
    }

    /// 置顶一个词在 `key` 下;同一键重复置顶覆盖旧值。
    pub fn pin(&mut self, key: &str, word: &str) {
        if key.is_empty() || word.is_empty() || self.pinned.get(key).is_some_and(|old| old == word)
        {
            return;
        }
        self.pinned.insert(key.to_string(), word.to_string());
        self.rev = self.rev.wrapping_add(1);
    }

    /// `key` 下的置顶词。
    pub fn pinned(&self, key: &str) -> Option<&str> {
        self.pinned.get(key).map(String::as_str)
    }

    /// 新键入表时建首字母变体与前缀。键只增不删,所以不维护删除。
    pub(crate) fn index_key(&mut self, key: &str) {
        for variant in variants(key) {
            for (at, _) in variant.match_indices('\'') {
                self.prefixes.insert(variant[..=at].to_string());
            }
            self.by_initials
                .entry(variant)
                .or_default()
                .push(key.to_string());
        }
    }

    /// 这组首字母串(见 `Lexicon` 的 `initials_of`)命中的学习键,排序去重,不扫描用户词库。
    pub(crate) fn matching_keys(&self, lookups: &[String]) -> Vec<&str> {
        let mut keys: Vec<&str> = lookups
            .iter()
            .filter_map(|lookup| self.by_initials.get(lookup))
            .flatten()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    /// 学习键对应的词条。
    pub(crate) fn entries(&self, key: &str) -> &[WordEntry] {
        self.words.get(key).map_or(&[], Vec::as_slice)
    }

    /// 这组首字母串能否继续接成更长的用户键(前缀命中即可,允许误报,不漏词)。
    pub(crate) fn may_extend(&self, lookups: &[String]) -> bool {
        lookups
            .iter()
            .any(|lookup| self.prefixes.contains(&format!("{lookup}'")))
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.raw.is_empty()
    }

    /// 词条总数。
    pub fn len(&self) -> usize {
        self.count
    }

    /// 模糊组的打字习惯学习:选中一个模糊读法时调用。
    pub fn learn_groups(&mut self, mask: u8) {
        self.typing.learn_groups(mask, now());
        self.rev = self.rev.wrapping_add(1);
    }

    /// 命中这些模糊组时给候选的加分;只降低模糊代价,精确首位保护不变。
    pub fn group_bonus(&self, mask: u8) -> f64 {
        self.typing.group_bonus(mask, now())
    }

    /// 设置页展示用:各模糊组当前有效次数。
    pub fn group_stats(&self) -> Vec<(&'static str, u32)> {
        self.typing.group_stats(now())
    }

    /// 用户在快捷触发键下选了字面原文而不是动作候选。
    pub fn learn_literal(&mut self, key: &str, action: &str) {
        self.typing.learn_literal(key, action, now());
        self.rev = self.rev.wrapping_add(1);
    }

    /// 快捷词被选择字面原文的有效次数;够多了就不再注入动作候选。
    pub fn literal_picks(&self, key: &str, action: &str) -> u32 {
        self.typing.literal_picks(key, action, now())
    }
}
