//! 用户词库的删除、查询、导出与导入。
//!
//! 与学习逻辑分开,避免迁移/序列化代码和评分公式互相淹没。

use std::collections::HashMap;

use super::*;

impl UserDict {
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
        self.word_keys.remove(word);
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
    pub(crate) fn forget_transitions(&mut self, word: &str) {
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
    pub(crate) fn forget_trigrams(&mut self, word: &str) {
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
    pub(crate) fn forget_apps(&mut self, word: &str) -> bool {
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

    /// 词条总数;设置窗导入后用它报「词库现在有多少条」。
    pub fn len(&self) -> usize {
        self.count
    }

    /// 只遍历音节数与首字母可能匹配的用户键。首字母模糊等价由调用方展开;
    /// 同一音节数、同一首字母是逐音节匹配的必要条件,不会漏词。
    pub fn for_each_matching(&self, shapes: &[(usize, char)], mut f: impl FnMut(&str, &str, u32)) {
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

    /// 当前输入是否还可能接成某个更长的用户词。只按音节数与首字母做保守判断:
    /// 命中形状里存在不短于当前单元数的键即可,允许误报,不允许漏掉用户词。
    pub fn may_extend(&self, syllables: usize, initials: &[char]) -> bool {
        self.by_shape.iter().any(|(&(count, first), keys)| {
            count >= syllables && initials.contains(&first) && !keys.is_empty()
        })
    }

    /// 导出为行文本:词条 `键<TAB>词<TAB>频次<TAB>时间戳`,
    /// 组级/字级/词级/三词/应用/跳过学习用 `@group`、`@context`、`@transition`、
    /// `@trigram`、`@app`、`@reject` 行;按内容排序保证文件稳定。
    pub fn export(&self) -> String {
        let mut out = String::new();
        for (key, entries) in in_key_order(&self.map) {
            let mut entries: Vec<&UserEntry> = entries.iter().collect();
            entries.sort_by(|a, b| a.word.cmp(&b.word));
            for e in entries {
                out.push_str(&format!("{key}\t{}\t{}\t{}\n", e.word, e.freq, e.ts));
            }
        }
        for (name, stat) in FUZZY_GROUPS.iter().zip(self.groups.iter()) {
            if stat.count > 0 {
                out.push_str(&format!("@group\t{name}\t{}:{}\n", stat.count, stat.ts));
            }
        }
        for ((left, right), stat) in in_key_order(&self.context) {
            if stat.count > 0 {
                out.push_str(&format!(
                    "@context\t{left}\t{right}\t{}\t{}\n",
                    stat.count, stat.ts
                ));
            }
        }
        for (left, words) in in_key_order(&self.word_ctx) {
            for (word, count) in in_key_order(words) {
                out.push_str(&format!("@wordctx\t{left}\t{word}\t{count}\n"));
            }
        }
        for (left, next) in in_key_order(&self.transitions) {
            for (right, stat) in in_key_order(next) {
                if stat.count > 0 {
                    out.push_str(&format!(
                        "@transition\t{left}\t{right}\t{}\t{}\n",
                        stat.count, stat.ts
                    ));
                }
            }
        }
        for (left, mids) in in_key_order(&self.trigrams) {
            for (mid, next) in in_key_order(mids) {
                for (right, stat) in in_key_order(next) {
                    if stat.count > 0 {
                        out.push_str(&format!(
                            "@trigram\t{left}\t{mid}\t{right}\t{}\t{}\n",
                            stat.count, stat.ts
                        ));
                    }
                }
            }
        }
        for (host, words) in in_key_order(&self.apps) {
            for (word, stat) in in_key_order(words) {
                if stat.count > 0 {
                    out.push_str(&format!(
                        "@app\t{host}\t{word}\t{}\t{}\n",
                        stat.count, stat.ts
                    ));
                }
            }
        }
        for (key, words) in in_key_order(&self.rejects) {
            for (word, stat) in in_key_order(words) {
                if stat.count > 0 {
                    out.push_str(&format!(
                        "@reject\t{key}\t{word}\t{}\t{}\n",
                        stat.count, stat.ts
                    ));
                }
            }
        }
        for (key, word) in in_key_order(&self.pinned) {
            out.push_str(&format!("@pin\t{key}\t{word}\n"));
        }
        out
    }

    /// 从 [`Self::export`] 的输出导入,坏行跳过,同键同词合并。
    pub fn import(&mut self, text: &str) {
        let mut changed = false;
        for line in text.lines() {
            let fields: Vec<&str> = line.split('\t').collect();
            changed |= self.import_fields(&fields);
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

    /// 按字段切片分派一行;字段数或取值不对的行返回 `false`。
    fn import_fields(&mut self, fields: &[&str]) -> bool {
        match *fields {
            ["@group", name, value] => self.import_group(name, value),
            ["@context", left, right, count, ts] => self.import_context(left, right, count, ts),
            ["@wordctx", left, word, count] => self.import_word_context(left, word, count),
            ["@transition", left, right, count, ts] => {
                self.import_transition(left, right, count, ts)
            }
            ["@trigram", left, mid, right, count, ts] => {
                self.import_trigram(left, mid, right, count, ts)
            }
            ["@app", host, word, count, ts] => self.import_app(host, word, count, ts),
            ["@reject", key, word, count, ts] => self.import_reject(key, word, count, ts),
            ["@pin", key, word] => self.import_pin(key, word),
            [key, word, freq, ts] if !is_control(key) => self.import_word(key, word, freq, ts),
            _ => false,
        }
    }

    fn import_group(&mut self, name: &str, value: &str) -> bool {
        let Some((count, ts)) = value.split_once(':') else {
            return false;
        };
        let Some((count, ts)) = stamp(count, ts) else {
            return false;
        };
        let Some(index) = FUZZY_GROUPS.iter().position(|group| *group == name) else {
            return false;
        };
        let stat = &mut self.groups[index];
        absorb(&mut stat.count, &mut stat.ts, count, ts);
        true
    }

    fn import_context(&mut self, left: &str, right: &str, count: &str, ts: &str) -> bool {
        let (Some(left), Some(right)) = (single_char(left), single_char(right)) else {
            return false;
        };
        let Some((count, ts)) = stamp(count, ts) else {
            return false;
        };
        if !is_hanzi(left) || !is_hanzi(right) || count == 0 {
            return false;
        }
        let stat = self.context.entry((left, right)).or_default();
        absorb(&mut stat.count, &mut stat.ts, count, ts);
        true
    }

    /// 导入按最大值合并,与其它计数一致:同一份文件导两次结果不变。
    fn import_word_context(&mut self, left: &str, word: &str, count: &str) -> bool {
        let Some(left) = single_char(left) else {
            return false;
        };
        let Ok(count) = count.parse::<u32>() else {
            return false;
        };
        let left_ok = left == NO_LEFT || is_hanzi(left);
        if !left_ok || count == 0 || word.is_empty() || !word.chars().all(is_hanzi) {
            return false;
        }
        let words = self.word_ctx.entry(left).or_default();
        let stat = words.entry(word.to_string()).or_insert(0);
        *stat = (*stat).max(count);
        true
    }

    fn import_transition(&mut self, left: &str, right: &str, count: &str, ts: &str) -> bool {
        let Some((count, ts)) = stamp(count, ts) else {
            return false;
        };
        if left.is_empty() || right.is_empty() || count == 0 {
            return false;
        }
        let stat = self
            .transitions
            .entry(left.to_string())
            .or_default()
            .entry(right.to_string())
            .or_default();
        absorb(&mut stat.count, &mut stat.ts, count, ts);
        true
    }

    fn import_trigram(
        &mut self,
        left: &str,
        mid: &str,
        right: &str,
        count: &str,
        ts: &str,
    ) -> bool {
        let Some((count, ts)) = stamp(count, ts) else {
            return false;
        };
        if left.is_empty() || mid.is_empty() || right.is_empty() || count == 0 {
            return false;
        }
        let stat = self
            .trigrams
            .entry(left.to_string())
            .or_default()
            .entry(mid.to_string())
            .or_default()
            .entry(right.to_string())
            .or_default();
        absorb(&mut stat.count, &mut stat.ts, count, ts);
        true
    }

    fn import_app(&mut self, host: &str, word: &str, count: &str, ts: &str) -> bool {
        let host = host.trim().to_ascii_lowercase();
        let Some((count, ts)) = stamp(count, ts) else {
            return false;
        };
        if host.is_empty() || word.is_empty() || count == 0 {
            return false;
        }
        let stat = self
            .apps
            .entry(host)
            .or_default()
            .entry(word.to_string())
            .or_default();
        absorb(&mut stat.count, &mut stat.ts, count, ts);
        true
    }

    fn import_reject(&mut self, key: &str, word: &str, count: &str, ts: &str) -> bool {
        let Some((count, ts)) = stamp(count, ts) else {
            return false;
        };
        if key.is_empty() || word.is_empty() || count == 0 {
            return false;
        }
        let stat = self
            .rejects
            .entry(key.to_string())
            .or_default()
            .entry(word.to_string())
            .or_default();
        absorb(&mut stat.count, &mut stat.ts, count, ts);
        true
    }

    fn import_pin(&mut self, key: &str, word: &str) -> bool {
        if key.is_empty() || word.is_empty() {
            return false;
        }
        self.pinned.insert(key.to_string(), word.to_string());
        true
    }

    /// 普通词条行:`键<TAB>词<TAB>频次<TAB>时间戳`,同键同词取较大的频次。
    fn import_word(&mut self, key: &str, word: &str, freq: &str, ts: &str) -> bool {
        let Some((freq, ts)) = stamp(freq, ts) else {
            return false;
        };
        if key.is_empty() || word.is_empty() {
            return false;
        }
        let new_key = !self.map.contains_key(key);
        let entries = self.map.entry(key.to_string()).or_default();
        match entries.iter_mut().find(|e| e.word == word) {
            Some(e) => {
                e.freq = e.freq.max(freq);
                e.ts = e.ts.max(ts);
            }
            None => {
                entries.push(UserEntry {
                    word: word.to_string(),
                    freq,
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

/// 按键排序后的一张表;导出的行顺序由此固定,与插入顺序无关。
fn in_key_order<K: Ord, V>(map: &HashMap<K, V>) -> Vec<(&K, &V)> {
    let mut items: Vec<(&K, &V)> = map.iter().collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    items
}

/// 同一条统计取较大的计数与较新的时间戳。
fn absorb(count: &mut u32, ts: &mut u64, new_count: u32, new_ts: u64) {
    *count = (*count).max(new_count);
    *ts = (*ts).max(new_ts);
}

/// 计数与时间戳两个字段;任一不是数字就整行作废。
fn stamp(count: &str, ts: &str) -> Option<(u32, u64)> {
    Some((count.parse().ok()?, ts.parse().ok()?))
}

/// 控制行的标记;这些前缀的行按控制行读,不会落进普通词条。
fn is_control(key: &str) -> bool {
    matches!(
        key,
        "@group" | "@context" | "@transition" | "@trigram" | "@app" | "@reject" | "@pin"
    )
}

#[cfg(test)]
#[path = "user_io_tests.rs"]
mod tests;
