//! 用户词库的删除、查询、导出与导入。
//!
//! 与学习逻辑分开,避免迁移/序列化代码和评分公式互相淹没。

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
