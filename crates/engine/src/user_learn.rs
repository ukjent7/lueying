//! 用户词库的学习与打分:词组、上下文、转移、三词窗口、应用偏好、跳过负分。
//!
//! 类型、字段与上限常量留在 `user.rs`,这里是子模块,可以直接访问私有字段。

use super::*;

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
        self.pinned.clear();
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

    pub(crate) fn transition_count(&self) -> usize {
        self.transitions.values().map(|next| next.len()).sum()
    }

    /// 保留最常见的前九成词对;一次性重建,摊到每次学习里的成本可忽略。
    pub(crate) fn prune_transitions(&mut self) {
        let keep = MAX_TRANSITIONS * 9 / 10;
        let mut all: Vec<(String, String, ContextStat)> = self
            .transitions
            .drain()
            .flat_map(|(left, next)| {
                next.into_iter()
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

    pub(crate) fn trigram_count(&self) -> usize {
        self.trigrams
            .values()
            .flat_map(|next| next.values())
            .map(|next| next.len())
            .sum()
    }

    /// 保留最常见的前九成三词窗口;一次性重建,摊到每次学习里的成本可忽略。
    pub(crate) fn prune_trigrams(&mut self) {
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

    pub(crate) fn app_count(&self) -> usize {
        self.apps.values().map(|words| words.len()).sum()
    }

    /// 保留最常见的前九成应用词;一次性重建,摊到每次学习里的成本可忽略。
    pub(crate) fn prune_apps(&mut self) {
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
    pub(crate) fn forgive(&mut self, key: &str, word: &str) {
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

    pub(crate) fn reject_count(&self) -> usize {
        self.rejects.values().map(|words| words.len()).sum()
    }

    /// 保留次数最多的前九成记录;一次性重建,摊到每次学习里的成本可忽略。
    pub(crate) fn prune_rejects(&mut self) {
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
    pub(crate) fn prune_context(&mut self) {
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

    /// 取消一个词的所有置顶(跨学习键);返回是否有变化。
    pub fn unpin_word(&mut self, word: &str) -> bool {
        let before = self.pinned.len();
        self.pinned.retain(|_, old| old != word);
        let changed = self.pinned.len() != before;
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
        changed
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
    pub(crate) fn prune(&mut self) {
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
}
