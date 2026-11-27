//! 用户词库的导出与导入。与学习逻辑分开。
//!
//! 文件是制表符分隔的行,首字段是记录类型,每行一条记录,按内容排序,导出稳定:
//!
//! - `w 键 词 次数 稳定度 最近时刻 误选时刻`:读音词记忆,误选时刻为 0 表示没有。
//! - `x 键 词 纠正次数 最近时刻`:非首选位置被主动选中的证据;紧跟在对应的 `w` 行后面。
//! - `f 键 词 次数 最近时刻 左邻串`:相同 `w` 版本的片段作用域,每两字一个左邻,可为空。
//! - `r 击键串 词 次数 稳定度 最近时刻`:原样输入记忆。
//! - `t 前词 后词 次数 稳定度 最近时刻`:词序记忆。
//! - `g 模糊组 次数 时刻 左邻串`:打字习惯,模糊组;左邻串与 `r` 同构,旧档案可无此列。
//! - `a 键 快捷动作 次数 时刻`:打字习惯,快捷词的字面选择。
//! - `p 键 词`:置顶。
//!
//! 数字用 Rust 默认的最短往返格式输出,导入后逐位相等。坏行整行跳过;
//! 同一条记录重复导入时取较新的那份,不累加。

use super::*;
use crate::user_typing::Weekly;
use lueying_dict::syllables::FUZZY_GROUPS;

impl UserDict {
    /// 导出为行文本。
    pub fn export(&self) -> String {
        let mut out = String::new();
        for (key, entries) in in_key_order(&self.words) {
            let mut entries: Vec<&WordEntry> = entries.iter().collect();
            entries.sort_by(|a, b| a.word.cmp(&b.word));
            for e in entries {
                let m = e.memory;
                out.push_str(&format!(
                    "w\t{key}\t{}\t{}\t{}\t{}\t{}\n",
                    e.word,
                    m.uses,
                    m.stability,
                    m.last,
                    e.miss.unwrap_or(0)
                ));
                if e.corrected.count > 0 {
                    out.push_str(&format!(
                        "x\t{key}\t{}\t{}\t{}\n",
                        e.word, e.corrected.count, e.corrected.last
                    ));
                }
                if let Some(scope) = &e.fragment_contexts {
                    let contexts: String = scope.iter().flatten().collect();
                    out.push_str(&format!(
                        "f\t{key}\t{}\t{}\t{}\t{contexts}\n",
                        e.word, m.uses, m.last
                    ));
                }
            }
        }
        for (typed, entries) in in_key_order(&self.raw) {
            let mut entries: Vec<&RawEntry> = entries.iter().collect();
            entries.sort_by(|a, b| a.word.cmp(&b.word));
            for e in entries {
                let m = e.memory;
                out.push_str(&format!(
                    "r\t{typed}\t{}\t{}\t{}\t{}",
                    e.word, m.uses, m.stability, m.last
                ));
                if let Some(contexts) = &e.contexts {
                    // 每个左邻一项:`*` 表示句首(无左邻),否则两个汉字。
                    let mut encoded = String::new();
                    for scope in contexts {
                        match scope {
                            Some([a, b]) => {
                                encoded.push(*a);
                                encoded.push(*b);
                            }
                            None => encoded.push('*'),
                        }
                    }
                    out.push_str(&format!("\t{encoded}"));
                }
                out.push('\n');
            }
        }
        for (prev, next) in in_key_order(&self.transitions) {
            for (word, m) in in_key_order(next) {
                out.push_str(&format!(
                    "t\t{prev}\t{word}\t{}\t{}\t{}\n",
                    m.uses, m.stability, m.last
                ));
            }
        }
        for (index, habit) in self.typing.groups.iter().enumerate() {
            if habit.stat.count > 0 {
                out.push_str(&format!(
                    "g\t{}\t{}\t{}",
                    FUZZY_GROUPS[index], habit.stat.count, habit.stat.ts
                ));
                if let Some(contexts) = &habit.contexts {
                    // 与 `r` 记录同构:每个左邻一项,`*` 表示句首(无左邻)。
                    let mut encoded = String::new();
                    for scope in contexts {
                        match scope {
                            Some([a, b]) => {
                                encoded.push(*a);
                                encoded.push(*b);
                            }
                            None => encoded.push('*'),
                        }
                    }
                    out.push_str(&format!("\t{encoded}"));
                }
                out.push('\n');
            }
        }
        for ((key, action), stat) in in_key_order(&self.typing.literal) {
            out.push_str(&format!(
                "a\t{key}\t{action}\t{}\t{}\n",
                stat.count, stat.ts
            ));
        }
        for (key, word) in in_key_order(&self.pinned) {
            out.push_str(&format!("p\t{key}\t{word}\n"));
        }
        out
    }

    /// 从 [`Self::export`] 的输出导入。坏行跳过;已有记录按较新的那份合并。
    pub fn import(&mut self, text: &str) {
        let mut changed = false;
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            changed |= self.import_fields(&fields);
        }
        if changed {
            self.rev = self.rev.wrapping_add(1);
        }
    }

    fn import_fields(&mut self, fields: &[&str]) -> bool {
        match *fields {
            ["w", key, word, uses, stability, last, miss] => {
                let (Some(memory), Some(miss)) =
                    (memory(uses, stability, last), miss.parse::<u64>().ok())
                else {
                    return false;
                };
                self.import_word(key, word, memory, (miss != 0).then_some(miss))
            }
            ["f", key, word, uses, last, contexts] => {
                let (Ok(uses), Ok(last)) = (uses.parse::<u32>(), last.parse::<u64>()) else {
                    return false;
                };
                let mut scope = BTreeSet::new();
                let mut chars = contexts.chars();
                while let Some(left) = chars.next() {
                    let Some(right) = chars.next() else {
                        return false;
                    };
                    if !is_context_han(left) || !is_context_han(right) {
                        return false;
                    }
                    scope.insert([left, right]);
                }
                let Some(entry) = self.entries_mut(key).iter_mut().find(|e| e.word == word) else {
                    return false;
                };
                if (entry.memory.last, entry.memory.uses) != (last, uses) {
                    return false;
                }
                entry
                    .fragment_contexts
                    .get_or_insert_with(BTreeSet::new)
                    .extend(scope);
                true
            }
            ["r", typed, word, uses, stability, last, ref rest @ ..] => {
                let Some(memory) = memory(uses, stability, last) else {
                    return false;
                };
                if typed.is_empty() || word.is_empty() {
                    return false;
                }
                let contexts = match rest {
                    [] => None,
                    [encoded] => {
                        let mut scope = BTreeSet::new();
                        let mut chars = encoded.chars();
                        while let Some(first) = chars.next() {
                            if first == '*' {
                                scope.insert(None);
                                continue;
                            }
                            let Some(second) = chars.next() else {
                                return false;
                            };
                            if !is_context_han(first) || !is_context_han(second) {
                                return false;
                            }
                            scope.insert(Some([first, second]));
                        }
                        Some(scope)
                    }
                    _ => return false,
                };
                let entries = self.raw.entry(typed.to_string()).or_default();
                match entries.iter_mut().find(|entry| entry.word == word) {
                    Some(entry) => {
                        absorb(&mut entry.memory, memory);
                        if let Some(scope) = contexts {
                            entry
                                .contexts
                                .get_or_insert_with(BTreeSet::new)
                                .extend(scope);
                        }
                    }
                    None => entries.push(RawEntry {
                        word: word.to_string(),
                        memory,
                        contexts,
                    }),
                }
                true
            }
            ["t", prev, word, uses, stability, last] => {
                let Some(memory) = memory(uses, stability, last) else {
                    return false;
                };
                if prev.is_empty() || word.is_empty() {
                    return false;
                }
                let slot = self
                    .transitions
                    .entry(prev.to_string())
                    .or_default()
                    .entry(word.to_string())
                    .or_default();
                absorb(slot, memory);
                true
            }
            ["g", name, count, ts, ref rest @ ..] => {
                let (Ok(count), Ok(ts)) = (count.parse::<u32>(), ts.parse::<u64>()) else {
                    return false;
                };
                let Some(index) = FUZZY_GROUPS.iter().position(|group| *group == name) else {
                    return false;
                };
                let contexts = match rest {
                    [] => None,
                    [encoded] => {
                        let mut scope = BTreeSet::new();
                        let mut chars = encoded.chars();
                        while let Some(first) = chars.next() {
                            if first == '*' {
                                scope.insert(None);
                                continue;
                            }
                            let Some(second) = chars.next() else {
                                return false;
                            };
                            if !is_context_han(first) || !is_context_han(second) {
                                return false;
                            }
                            scope.insert(Some([first, second]));
                        }
                        Some(scope)
                    }
                    _ => return false,
                };
                let habit = &mut self.typing.groups[index];
                if ts >= habit.stat.ts {
                    habit.stat = Weekly { count, ts };
                }
                if let Some(scope) = contexts {
                    habit
                        .contexts
                        .get_or_insert_with(BTreeSet::new)
                        .extend(scope);
                }
                true
            }
            ["a", key, action, count, ts] => {
                let (Ok(count), Ok(ts)) = (count.parse::<u32>(), ts.parse::<u64>()) else {
                    return false;
                };
                if key.is_empty() || action.is_empty() {
                    return false;
                }
                let stat = self
                    .typing
                    .literal
                    .entry((key.to_string(), action.to_string()))
                    .or_default();
                if ts >= stat.ts {
                    *stat = Weekly { count, ts };
                }
                true
            }
            ["x", key, word, count, last] => {
                let (Ok(count), Ok(last)) = (count.parse::<u32>(), last.parse::<u64>()) else {
                    return false;
                };
                if count == 0 || key.is_empty() || word.is_empty() {
                    return false;
                }
                // `x` 只给已存在的 `w` 行补纠正证据;导入顺序由导出保证。
                let Some(entry) = self.entries_mut(key).iter_mut().find(|e| e.word == word) else {
                    return false;
                };
                if last >= entry.corrected.last {
                    entry.corrected = Corrected { count, last };
                }
                true
            }
            ["p", key, word] => {
                if key.is_empty() || word.is_empty() {
                    return false;
                }
                self.pinned.insert(key.to_string(), word.to_string());
                true
            }
            _ => false,
        }
    }

    fn import_word(&mut self, key: &str, word: &str, memory: Memory, miss: Option<u64>) -> bool {
        if key.is_empty() || word.is_empty() {
            return false;
        }
        let new_key = !self.words.contains_key(key);
        let entries = self.words.entry(key.to_string()).or_default();
        match entries.iter_mut().find(|e| e.word == word) {
            Some(entry) => {
                let old = (entry.memory.last, entry.memory.uses);
                absorb(&mut entry.memory, memory);
                if (entry.memory.last, entry.memory.uses) != old {
                    entry.fragment_contexts = None;
                }
                entry.miss = entry.miss.max(miss);
            }
            None => {
                entries.push(WordEntry {
                    word: word.to_string(),
                    memory,
                    miss,
                    corrected: Corrected::default(),
                    fragment_contexts: None,
                });
                self.count += 1;
            }
        }
        if new_key {
            self.index_key(key);
        }
        if self
            .word_keys
            .get(word)
            .is_none_or(|old| old.len() < key.len())
        {
            self.word_keys.insert(word.to_string(), key.to_string());
        }
        true
    }
}

/// 解析一条记忆;次数、稳定度或时刻不合法时返回 `None`。
fn memory(uses: &str, stability: &str, last: &str) -> Option<Memory> {
    let memory = Memory {
        uses: uses.parse().ok()?,
        stability: stability.parse().ok()?,
        last: last.parse().ok()?,
    };
    (memory.stability.is_finite() && memory.stability > 0.0).then_some(memory)
}

/// 同一条记忆取较新的那份:最近使用时刻晚的优先,时刻相同取次数多的。
fn absorb(slot: &mut Memory, incoming: Memory) {
    if (incoming.last, incoming.uses) > (slot.last, slot.uses) {
        *slot = incoming;
    }
}

/// 按键排序后的一张表;导出的行顺序由此固定,与插入顺序无关。
fn in_key_order<K: Ord, V>(map: &HashMap<K, V>) -> Vec<(&K, &V)> {
    let mut items: Vec<(&K, &V)> = map.iter().collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    items
}

#[cfg(test)]
#[path = "user_io_tests.rs"]
mod tests;
