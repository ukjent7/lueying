//! 词图查询:词典命中、用户词、模糊扩展与纠错键的合并。
//!
//! `Querier` 与缓存字段留在 `query.rs`,这里是子模块;跨文件调用的方法
//! 标为 `pub(crate)` 供 `Builder` 使用。

use super::*;
use lueying_dict::signature;

impl Querier<'_> {
    pub(crate) fn lookup(&mut self, sylls: &[Syll]) -> Rc<Vec<Hit>> {
        let key = typed_key(sylls);
        if let Some(hits) = self.cache.get(&key) {
            return Rc::clone(hits);
        }
        let (hits, users) = self.compute(sylls, &key);
        self.user_cache.insert(key.clone(), Rc::new(users));
        let hits = Rc::new(hits);
        self.cache.insert(key, Rc::clone(&hits));
        hits
    }

    pub(crate) fn compute(
        &mut self,
        sylls: &[Syll],
        key: &str,
    ) -> (Vec<Hit>, HashMap<String, UserMatch>) {
        let any_abbrev = sylls.iter().any(|s| s.abbrev);
        let fuzzy_on = !self.fuzzy.is_off();
        // 用户词先按音节数与首字母收窄,再做逐音节匹配。词典命中也要用这份
        // 结果提频:同一个词用简拼学过,换成全拼输入时仍然应该靠前。
        let mut users: HashMap<String, UserMatch> = HashMap::new();
        if !self.user.is_empty() {
            let first = sylls
                .first()
                .and_then(|s| s.text.chars().next())
                .unwrap_or('\0');
            let shapes: Vec<(usize, char)> = syllables::initial_alternatives(first, &self.fuzzy)
                .into_iter()
                .map(|c| (sylls.len(), c))
                .collect();
            let user_dict = self.user;
            let fuzzy = self.fuzzy;
            let exp_cache = &mut self.exp_cache;
            user_dict.for_each_matching(&shapes, |ukey, uword, ufreq| {
                let Some((edits, groups)) = match_edits(ukey, sylls, true, &fuzzy, exp_cache)
                else {
                    return;
                };
                if let Some(current) = users.get_mut(uword) {
                    if ufreq > current.freq || (ufreq == current.freq && edits < current.edits) {
                        current.freq = ufreq;
                        current.edits = edits;
                        current.groups = groups;
                        current.key.clear();
                        current.key.push_str(ukey);
                    }
                } else {
                    users.insert(
                        uword.to_string(),
                        UserMatch {
                            freq: ufreq,
                            edits,
                            groups,
                            key: ukey.to_string(),
                        },
                    );
                }
            });
        }
        let mut hits: Vec<Hit> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        // 精确整键走专用索引:最快,也是 `trusted` 的证据来源。
        if !any_abbrev {
            for e in self.dict.lookup_exact(key) {
                let word = e.word.to_string();
                if seen.insert((word.clone(), e.key.to_string())) {
                    hits.push(Hit::dict(
                        word,
                        e.freq,
                        user_freq(&users, e.word),
                        self.total,
                        0,
                        true,
                        0,
                        None,
                    ));
                }
            }
        }
        // 简拼走签名区间;全拼模糊按等价音节枚举精确键。后者只做几次二分,
        // 不用为每个首字母翻转扫整段签名,长句输入也保持首键延迟。
        if any_abbrev {
            let base = signature(key);
            for sig in self.sig_variants(&base, sylls) {
                for e in self.dict.lookup_sig(&sig, SCAN_VISIT_CAP) {
                    if !seen.insert((e.word.to_string(), e.key.to_string())) {
                        continue;
                    }
                    let Some((edits, groups)) =
                        match_edits(e.key, sylls, false, &self.fuzzy, &mut self.exp_cache)
                    else {
                        continue;
                    };
                    hits.push(Hit::dict(
                        e.word.to_string(),
                        e.freq,
                        user_freq(&users, e.word),
                        self.total,
                        edits,
                        edits == 0,
                        groups,
                        (edits > 0).then(|| e.key.replace('\'', " ")),
                    ));
                }
            }
        } else if fuzzy_on {
            for (variant, edits, groups) in self.expanded_keys(sylls) {
                if variant == key {
                    continue;
                }
                for e in self.dict.lookup_exact(&variant) {
                    if !seen.insert((e.word.to_string(), e.key.to_string())) {
                        continue;
                    }
                    hits.push(Hit::dict(
                        e.word.to_string(),
                        e.freq,
                        user_freq(&users, e.word),
                        self.total,
                        edits,
                        false,
                        groups,
                        Some(e.key.replace('\'', " ")),
                    ));
                }
            }
        }
        // 用户词并入:词典已有的词已带上提频,只补词典没有的。
        let total = self.total;
        let mut hit_words: HashSet<String> = hits.iter().map(|h| h.word.clone()).collect();
        for (uword, m) in &users {
            if hit_words.insert(uword.clone()) {
                let mut hit = Hit::user_only(uword.clone(), m.freq, total, m.edits, m.groups);
                if m.edits > 0 {
                    hit.hint = Some(m.key.replace('\'', " "));
                }
                hits.push(hit);
            }
        }
        // 组级学习只降低该组的模糊罚分,不改精确命中的 `trusted` 保护。
        for hit in &mut hits {
            if hit.groups != 0 {
                hit.score += self.user.group_bonus(hit.groups);
            }
        }
        (hits, users)
    }

    /// 当前输入是否是某个词典键的逐字前缀。末音节没打完时键里没有结尾
    /// 隔音符,`has_key_prefix` 仍能命中,用来避免把半截输入当成手误。
    pub(crate) fn is_typing_prefix(&mut self, sylls: &[Syll]) -> bool {
        let key = typed_key(sylls);
        if self.dict.has_key_prefix(&key) {
            return true;
        }
        if self.fuzzy.is_off() {
            return false;
        }
        let keys = self.expanded_keys(sylls);
        keys.iter().any(|(key, _, _)| self.dict.has_key_prefix(key))
    }

    /// 还能不能接更长的词:用键/签名前缀判断,避免无意义的组合爆炸。
    /// 模糊等价音会改变键面,所以逐一检查等价键前缀。
    pub(crate) fn may_extend(&mut self, sylls: &[Syll]) -> bool {
        // 用户词可能不在主词典的前缀索引里。只查主词典会在用户独有词的
        // 首个音节之后剪枝,整串查询永远命中不了(如「日期」的 `ri'qi`)。
        if let Some(first) = sylls.first().and_then(|s| s.text.chars().next()) {
            let initials = syllables::initial_alternatives(first, &self.fuzzy);
            if self.user.may_extend(sylls.len(), &initials) {
                return true;
            }
        }
        if sylls.iter().any(|s| s.abbrev) {
            let key = typed_key(sylls);
            return self
                .sig_variants(&signature(&key), sylls)
                .iter()
                .any(|sig| self.dict.has_sig_prefix(sig));
        }
        let mut prefix = typed_key(sylls);
        prefix.push('\'');
        if self.dict.has_key_prefix(&prefix) {
            return true;
        }
        if self.fuzzy.is_off() {
            return false;
        }
        let keys = self.expanded_keys(sylls);
        keys.iter().any(|(key, _, _)| {
            let mut prefix = key.clone();
            prefix.push('\'');
            self.dict.has_key_prefix(&prefix)
        })
    }

    /// 全拼模糊召回:逐音节展开等价音节,组合成精确键再查主索引。
    ///
    /// 只做字符串组合与二分,不像签名扫描那样把同首字母的整段词条都读一遍。
    /// 组合数超过 [`MAX_FUZZY_KEYS`] 时按改动处数从少到多截断,保证原键与
    /// 单点模糊仍在。
    pub(crate) fn expanded_keys(&mut self, sylls: &[Syll]) -> Vec<(String, usize, u8)> {
        if sylls.iter().any(|s| s.abbrev) {
            return vec![(typed_key(sylls), 0, 0)];
        }
        let fuzzy = self.fuzzy;
        let cap = if self.long {
            MAX_FUZZY_KEYS_LONG
        } else {
            MAX_FUZZY_KEYS
        };
        let mut out = vec![(String::new(), 0usize, 0u8)];
        for t in sylls {
            let exps = self
                .exp_cache
                .entry(t.text.clone())
                .or_insert_with(|| Rc::new(syllables::expansions_scored(&t.text, &fuzzy)));
            let mut next = Vec::with_capacity(out.len() * exps.len());
            for (prefix, cost, groups) in &out {
                for (text, extra, group) in exps.iter() {
                    let mut key = String::with_capacity(prefix.len() + text.len() + 1);
                    key.push_str(prefix);
                    if !prefix.is_empty() {
                        key.push('\'');
                    }
                    key.push_str(text);
                    next.push((key, cost + extra, *groups | *group));
                }
            }
            out = next;
            if out.len() > cap {
                out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
                out.truncate(cap);
            }
        }
        out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        out.dedup_by(|a, b| {
            if a.0 == b.0 {
                b.2 |= a.2;
                true
            } else {
                false
            }
        });
        out
    }

    /// 改首字母的模糊组会改变签名:枚举各位置的首字母等价集。
    ///
    /// 单点翻转全部保留;多点组合最多 [`MAX_SIG_VARIANTS`] 个,超出后
    /// 只保留原键与单点翻转,保证四个以上音节的输入仍能召回一次模糊。
    pub(crate) fn sig_variants(&self, base: &str, sylls: &[Syll]) -> Vec<String> {
        let base_chars: Vec<char> = base.chars().collect();
        if self.fuzzy.is_off() || base_chars.len() != sylls.len() {
            return vec![base.to_string()];
        }
        let mut positions: Vec<(usize, Vec<char>)> = Vec::new();
        for (i, s) in sylls.iter().enumerate() {
            let Some(c) = s.text.chars().next() else {
                continue;
            };
            let alternatives = syllables::initial_alternatives(c, &self.fuzzy);
            if alternatives.len() > 1 {
                positions.push((i, alternatives));
            }
        }
        if positions.is_empty() {
            return vec![base.to_string()];
        }
        let mut out = vec![base.to_string()];
        for (i, alternatives) in &positions {
            let mut next = Vec::new();
            for sig in &out {
                for &c in alternatives {
                    if sig.chars().nth(*i) == Some(c) {
                        continue;
                    }
                    let mut chars: Vec<char> = sig.chars().collect();
                    chars[*i] = c;
                    next.push(chars.into_iter().collect::<String>());
                }
            }
            out.extend(next);
            out.sort();
            out.dedup();
            if out.len() > MAX_SIG_VARIANTS {
                // 预算用尽:退回原键 + 每个位置的单点翻转。
                let mut capped = vec![base.to_string()];
                for (pos, alternatives) in &positions {
                    for &c in alternatives {
                        if c == base_chars[*pos] {
                            continue;
                        }
                        let mut chars = base_chars.clone();
                        chars[*pos] = c;
                        capped.push(chars.into_iter().collect());
                    }
                }
                capped.sort();
                capped.dedup();
                return capped;
            }
        }
        out
    }
}
