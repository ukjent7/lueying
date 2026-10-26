//! 词图枚举与 Viterbi 解码:从音节图生成候选路径。
//!
//! `Builder` 只在这里使用;查询缓存仍由 `query.rs` 的 `Querier` 持有。

use super::*;

/// 词枚举的游标状态。`finish[p]` 表示 `raw[p..]` 能否被音节图完整切分:
/// 词尾落在切不动的位置(如 `si` 里只认 `s` 的「是」)不产生候选,
/// 残留的字母永远无法上屏。
struct Builder<'a, 'd> {
    q: &'a mut Querier<'d>,
    edges: &'a [Vec<Syll>],
    raw: &'a str,
    finish: &'a [bool],
    by_end: &'a mut [Vec<Node>],
    units: Vec<Syll>,
    spans: Vec<(usize, usize)>,
    visits: usize,
    safe_extend: bool,
    typing: bool,
    /// 整串能只用全拼音节读完。此时混入的简拼单元只可能是切分副作用,
    /// 按单元数扣分,让完整拼音读法优先。
    full_pinyin: bool,
}

impl Builder<'_, '_> {
    /// 从 `pos` 起深度优先枚举词,写到 `by_end[词尾]`。
    fn run(&mut self, root: usize, pos: usize) {
        if !self.units.is_empty() {
            if !self.finish[pos] {
                return;
            }
            let hits = self.q.lookup(&self.units);
            let abbrev = if self.full_pinyin {
                self.units.iter().filter(|s| s.abbrev).count()
            } else {
                0
            };
            let penalty = ABBREV_PENALTY * abbrev as f64;
            for h in hits.iter() {
                self.by_end[pos].push(Node {
                    start: root,
                    word: h.word.clone(),
                    score: h.score() - penalty,
                    spans: self.spans.clone(),
                    trusted: h.trusted,
                    edits: h.edits,
                    freq: h.freq,
                    user_freq: h.user_freq,
                    groups: h.groups,
                    hint: h.hint.clone(),
                });
            }
            self.visits += 1;
            // 只有从输入开头起的路径才算「还在打这个词」;后段的合法前缀
            // (如 hoa 末尾的 a)不能挡住整串的手误纠正。
            if root == 0 && pos == self.raw.len() && self.q.is_typing_prefix(&self.units) {
                self.typing = true;
            }
            if self.units.len() >= MAX_WORD_SYL || self.visits > NODE_BUDGET {
                return;
            }
            if self.q.may_extend(&self.units) {
                if root == 0
                    && pos == self.raw.len()
                    && (self.units.last().is_some_and(|s| !s.abbrev)
                        || self.units.iter().all(|s| s.abbrev))
                {
                    self.safe_extend = true;
                }
            } else {
                return;
            }
        }
        if pos >= self.raw.len() {
            return;
        }
        if self.raw.as_bytes()[pos] == b'\'' {
            // 隔音符是硬边界:词可以跨过它,但不产生音节。
            self.run(root, pos + 1);
            return;
        }
        for i in 0..self.edges[pos].len() {
            let e = self.edges[pos][i].clone();
            self.units.push(e.clone());
            self.spans.push(e.span);
            self.run(root, e.span.1);
            self.spans.pop();
            self.units.pop();
        }
    }
}

pub(crate) fn build_exact(
    dict: &Dictionary,
    user: &UserDict,
    fuzzy: &Fuzzy,
    gram: Option<&Gram>,
    cache: &mut HashMap<(String, String, bool), f64>,
    context: &str,
    host: &str,
    previous2: Option<&str>,
    previous: Option<&str>,
    raw: &str,
) -> Built {
    let total = dict.total_weight() as f64;
    let mut q = Querier {
        dict,
        user,
        fuzzy: *fuzzy,
        total,
        cache: HashMap::new(),
        user_cache: HashMap::new(),
        exp_cache: HashMap::new(),
    };
    let edges = segment::edges(raw);
    // finish[p]:raw[p..] 能否被音节图切完。词尾落在切不动的位置时,剩下
    // 的字母永远上不了屏,这类词节点直接不要。
    let mut finish = vec![false; raw.len() + 1];
    finish[raw.len()] = true;
    for i in (0..raw.len()).rev() {
        let ok = if raw.as_bytes()[i] == b'\'' {
            finish[i + 1]
        } else {
            edges[i].iter().any(|e| finish[e.span.1])
        };
        finish[i] = ok;
    }
    let mut by_end: Vec<Vec<Node>> = vec![Vec::new(); raw.len() + 1];
    let safe_extend;
    let typing;
    {
        let mut b = Builder {
            q: &mut q,
            edges: &edges,
            raw,
            finish: &finish,
            by_end: &mut by_end,
            units: Vec::new(),
            spans: Vec::new(),
            visits: 0,
            safe_extend: false,
            typing: false,
            full_pinyin: segment::complete_full(raw),
        };
        for (i, &ok) in finish.iter().take(raw.len()).enumerate() {
            if raw.as_bytes()[i] == b'\'' || !ok {
                continue;
            }
            b.units.clear();
            b.spans.clear();
            b.run(i, i);
        }
        safe_extend = b.safe_extend;
        typing = b.typing;
    }
    for list in &mut by_end {
        cap_nodes(list);
    }

    let mut gram = Grammar { gram, cache };
    let mut best: Vec<Vec<State>> = vec![Vec::new(); raw.len() + 1];
    best[0].push(State {
        score: 0.0,
        path: Vec::new(),
        trusted: true,
        sentence: false,
        edits: 0,
        freq: 0,
        groups: 0,
        hint: None,
    });
    for j in 1..=raw.len() {
        let mut cands: Vec<State> = Vec::new();
        // 隔音符不占音节:让上一位置的状态直接越过它,词与词才能在 `'` 两侧相接。
        if raw.as_bytes()[j - 1] == b'\'' {
            cands.extend(best[j - 1].iter().cloned());
        }
        for node in &by_end[j] {
            for st in &best[node.start] {
                let tail = context_tail(&st.path);
                let mut path = st.path.clone();
                path.push((node.word.clone(), node.spans.clone()));
                cands.push(State {
                    score: st.score + node.score + gram.bonus(&tail, &node.word, j == raw.len()),
                    path,
                    // 只有单节点整句算词典精确命中;多词拼出的句子没有这份依据。
                    trusted: st.path.is_empty() && node.trusted,
                    sentence: !st.path.is_empty() && st.edits + node.edits == 0,
                    edits: st.edits + node.edits,
                    freq: if st.path.is_empty() { node.freq } else { 0 },
                    groups: st.groups | node.groups,
                    hint: if st.path.is_empty() {
                        node.hint.clone()
                    } else {
                        None
                    },
                });
            }
        }
        cands.sort_by(|a, b| b.score.total_cmp(&a.score));
        cands.truncate(BEAM);
        best[j] = cands;
    }

    let mut full: Vec<Candidate> = Vec::new();
    for st in &best[raw.len()] {
        let text: String = st.path.iter().map(|p| p.0.as_str()).collect();
        if text.is_empty() {
            continue;
        }
        let spans: Vec<(usize, usize)> = st.path.iter().flat_map(|p| p.1.iter().copied()).collect();
        let key = spans_key(raw, &spans);
        let reject = user.reject_penalty(&key, &text);
        let matched = q.user_cache.get(&key).and_then(|users| users.get(&text));
        let user_freq = matched.map_or(0, |m| m.freq);
        let learned = matched.is_some_and(|m| m.edits == 0);
        full.push(Candidate {
            text: text.clone(),
            words: st.path.iter().map(|p| p.0.clone()).collect(),
            consume: spans.len(),
            score: st.score - reject,
            spans,
            full: true,
            trusted: st.trusted,
            sentence: st.sentence,
            edits: st.edits,
            freq: st.freq,
            user_freq,
            learned,
            groups: st.groups,
            hint: st.hint.clone(),
            learn_key: None,
            quick: false,
            rejected: reject > 0.0,
        });
    }
    // 个人上下文只调整整句候选的顺序:词频、模糊罚分和语法模型已经决定了
    // 进入 beam 的路径,这里再让用户常打的前后搭配与词序在同一批路径里靠前。
    for cand in &mut full {
        cand.score += user.context_bonus(context, &cand.text);
        cand.score += user.app_bonus(host, &cand.words);
        cand.score += user.transition_bonus(previous, &cand.words);
        cand.score += user.trigram_bonus(previous2, previous, &cand.words);
    }
    full.sort_by(|a, b| b.score.total_cmp(&a.score));
    // 同一句话可能由多种切分拼出,上屏文本完全相同;先按文本去重再占首页
    // 名额,否则重复整句会把前缀词挤进第一页,逐段上屏入口的位置忽前忽后。
    // 文本相同即用户可见结果相同,信任与学习标记取并集,别让去重丢掉整词证据。
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut unique: Vec<Candidate> = Vec::with_capacity(full.len());
    for cand in full {
        if let Some(&index) = at.get(&cand.text) {
            let kept = &mut unique[index];
            kept.trusted |= cand.trusted;
            kept.learned |= cand.learned;
            kept.freq = kept.freq.max(cand.freq);
            kept.user_freq = kept.user_freq.max(cand.user_freq);
            continue;
        }
        at.insert(cand.text.clone(), unique.len());
        unique.push(cand);
    }
    full = unique;
    // 单词候选:只取从输入开头起的词(选中后剩余输入继续组合)。
    let mut words: Vec<Candidate> = Vec::new();
    for (end, nodes) in by_end.iter().enumerate() {
        for node in nodes {
            if node.start != 0 {
                continue;
            }
            let key = spans_key(raw, &node.spans);
            let reject = user.reject_penalty(&key, &node.word);
            let matched = q
                .user_cache
                .get(&key)
                .and_then(|users| users.get(&node.word));
            let learned = matched.is_some_and(|m| m.edits == 0);
            words.push(Candidate {
                text: node.word.clone(),
                words: vec![node.word.clone()],
                consume: node.spans.len(),
                score: node.score - reject,
                spans: node.spans.clone(),
                full: end == raw.len(),
                trusted: node.trusted,
                sentence: false,
                edits: node.edits,
                freq: node.freq,
                user_freq: node.user_freq,
                learned,
                groups: node.groups,
                hint: node.hint.clone(),
                learn_key: None,
                quick: false,
                rejected: reject > 0.0,
            });
        }
    }
    // 前缀词也吃同一份个人上下文,逐段上屏时第一段就能按习惯排序。
    for cand in &mut words {
        cand.score += user.context_bonus(context, &cand.text);
        cand.score += user.app_bonus(host, &cand.words);
        cand.score += user.transition_bonus(previous, &cand.words);
        cand.score += user.trigram_bonus(previous2, previous, &cand.words);
    }
    words.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            // 同一个词既能按完整音节也能按简拼命中时,优先吃满音节:
            // `haoyi` 里的「好」应上屏 hao,不能只吃掉 h 留下 aoyi。
            .then(
                b.spans
                    .last()
                    .map_or(0, |s| s.1)
                    .cmp(&a.spans.last().map_or(0, |s| s.1)),
            )
            .then(a.text.cmp(&b.text))
    });
    // 首页优先放满整句备选:长句的正确读法常排在第 6-8 条,第二位的分段
    // 前缀会把它挤出页外。五条名额先给整句,最高分前缀词顺延到第 6 位;
    // 需要逐段上屏时下移一格即可,整句不足五条时前缀词自动补位。
    let mut cands: Vec<Candidate> = Vec::new();
    let mut rest = full.into_iter();
    cands.extend(rest.by_ref().take(5));
    let taken: HashSet<String> = cands.iter().map(|c| c.text.clone()).collect();
    let mut words = words.into_iter();
    // 第 6 位的分段入口不能与已收录的整句重复;重复项留着也会被去重掉。
    if let Some(prefix) = words.find(|w| !taken.contains(&w.text)) {
        cands.push(prefix);
    }
    cands.extend(rest);
    cands.extend(words);
    // 同一文本、同样吃完当前输入的候选对用户完全等价:上屏文字与剩余输入
    // 都相同,只是切分方式不同。保留分数最高的一条;吃完长度不同的前缀词
    // 仍然各自保留(例如「今天」既可能是整句也可能只吃掉前半段)。
    let mut seen: HashSet<(String, usize)> = HashSet::new();
    cands.retain(|c| {
        let cut = c.spans.last().map_or(0, |s| s.1);
        seen.insert((c.text.clone(), cut))
    });
    protect_exact(&mut cands);
    cands.truncate(MAX_CANDS);
    Built {
        cands,
        safe_extend,
        typing_prefix: typing,
    }
}
