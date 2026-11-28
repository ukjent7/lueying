//! 引擎的会话状态、配置、上下文与候选选择。
//!
//! `Engine` 的类型和字段留在 crate 根,这里放不涉及按键状态机的 inherent
//! impl;根模块的私有字段对子模块可见,不需要为了拆文件放宽公开 API。

use super::*;

impl Engine {
    pub fn new(dict: std::sync::Arc<Dictionary>, cfg: Config) -> Self {
        Self {
            dict,
            gram: None,
            charts: Default::default(),
            user: Arc::new(RwLock::new(UserDict::new())),
            learning: true,
            cfg,
            mode: Mode::Chinese,
            fullwidth: false,
            comp: None,
            comp_committed: false,
            phrase: None,
            cands: Vec::new(),
            hl: 0,
            page: 0,
            dq_open: false,
            sq_open: false,
            after_digit: false,
            context: String::new(),
            context_handled: 0,
            quick_handled: 0,
            last_word: None,
            last_commit: None,
            last_pick: None,
            host: String::new(),
            transition_handled: 0,
            now: None,
            rev: 0,
            modules: Arc::new(module::Modules::new()),
            scheme: Arc::new(scheme::FullPinyin),
            action: None,
            typing: Default::default(),
        }
    }

    /// 挂上宿主组合好的功能模块(链式构造)。
    pub fn with_modules(mut self, modules: Arc<module::Modules>) -> Self {
        self.modules = modules;
        self
    }

    /// 挂上输入方案(链式构造)。默认全拼。
    pub fn with_scheme(mut self, scheme: Arc<dyn scheme::Scheme>) -> Self {
        self.scheme = scheme;
        self
    }

    /// 运行中切换方案。组合尚未落定时直接清掉,避免旧击键按新规则误读。
    pub fn set_scheme(&mut self, scheme: Arc<dyn scheme::Scheme>) {
        if self.scheme.manifest().id == scheme.manifest().id {
            return;
        }
        self.scheme = scheme;
        if self.comp.is_some() {
            self.clear_comp();
        }
    }

    /// 挂上语言模型(链式构造;Server 启动时按文件是否存在决定)。
    pub fn with_gram(mut self, gram: std::sync::Arc<Gram>) -> Self {
        self.gram = Some(gram);
        self
    }

    /// 与其他会话共用用户词库(链式构造;Server 里所有宿主共用一份)。
    pub fn with_user(mut self, user: SharedUser) -> Self {
        self.user = user;
        self
    }

    /// 宿主注入当前本地时间;日期/时间/星期候选按它生成,不依赖时区库。
    pub fn set_now(&mut self, now: CivilTime) {
        self.now = Some(now);
    }

    /// 私密字段里关掉用户词库/自动造词学习。切回普通字段时恢复。
    pub fn set_learning(&mut self, enabled: bool) {
        if self.learning != enabled {
            self.learning = enabled;
            // 私密/普通字段切换:前一段文本、引号配对和词序都不能带进新字段。
            self.clear_context();
            self.phrase = None;
        }
    }

    /// 记录宿主 exe 名,供功能模块读取;空名表示未知宿主。
    pub fn set_host(&mut self, host: &str) {
        self.host = host.trim().to_ascii_lowercase();
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        // 数字状态跟着文档走,不随中/英切换重置:英文态直通 `3` 后切回中文,
        // 紧接着的 `.` 仍应按小数点处理。
        if mode == Mode::English {
            self.clear_comp();
        }
    }

    pub fn fullwidth(&self) -> bool {
        self.fullwidth
    }

    /// 全角只改后续按键的输出,不落定或改写已经敲了一半的拼音。
    pub fn set_fullwidth(&mut self, fullwidth: bool) {
        self.fullwidth = fullwidth;
    }

    pub fn composition(&self) -> Option<&Composition> {
        self.comp.as_ref()
    }

    pub fn candidates(&self) -> Vec<String> {
        self.cands.iter().map(|c| c.text.clone()).collect()
    }

    /// 带来源标记的候选。重排只应把 `full` 候选移到首位;`trusted` 是
    /// 是否由词典精确回答整个按键,`edits` 是模糊改动处数。
    pub fn candidates_detailed(&self) -> &[Candidate] {
        &self.cands
    }

    /// 候选集合版本号。异步结果只在版本号未变时落地。
    pub fn revision(&self) -> u64 {
        self.rev
    }

    /// 当前组合里是否已经上屏过分段词。
    pub fn composition_committed(&self) -> bool {
        self.comp_committed
    }

    /// 上屏后立即取下一词建议。没有组合也能调用：读取最近上屏的词与用户词序，
    /// 返回最多 `limit` 条不同词。首位按用户实际接过的强度排序，再优先覆盖
    /// 不同首字，避免 N 条建议全是同一个开头。
    pub fn predict_next(&self, limit: usize) -> Vec<NextWord> {
        let Some(previous) = self.last_word.as_deref() else {
            return Vec::new();
        };
        self.predict_after(previous, limit)
    }

    /// 指定前词的下一词建议。宿主在组合结束后拿着刚落定的词调用，模拟与
    /// 测试也用它逐词评估；与 [`Self::predict_next`] 共用同一份用户词序。
    /// 先每个首字取一条最强的，再补齐其余词，避免 N 条建议挤在同一个开头。
    pub fn predict_after(&self, previous: &str, limit: usize) -> Vec<NextWord> {
        if limit == 0 || previous.is_empty() {
            return Vec::new();
        }
        let user = self.user.read().unwrap_or_else(|p| p.into_inner());
        let words = user.predict_next(Some(previous), limit.saturating_mul(4).max(limit));
        let mut out: Vec<NextWord> = Vec::with_capacity(limit);
        let mut firsts = std::collections::HashSet::new();
        for diverse in [true, false] {
            for word in &words {
                if out.len() >= limit {
                    break;
                }
                let first = word.chars().next().unwrap_or('\0');
                if diverse && !firsts.insert(first) {
                    continue;
                }
                if out.iter().any(|known| known.text == *word) {
                    continue;
                }
                out.push(NextWord {
                    text: word.clone(),
                    key: user.key_for_word(word).map(str::to_string),
                });
            }
        }
        out.truncate(limit);
        out
    }

    /// 最近上屏的文本(最多 [`crate::CONTEXT_KEEP`] 字)。
    pub fn context(&self) -> &str {
        &self.context
    }

    /// 用光标前文本替换上下文,只保留末尾 [`crate::CONTEXT_KEEP`] 字。
    ///
    /// TSF 在组合开始时上报左邻文本;它包含之前所有已上屏内容,因此可以整体替换。
    pub fn set_context(&mut self, text: &str) {
        self.context.clear();
        self.push_context(text);
        // 焦点切回后引号奇偶不能沿用上个文档;从左邻文本里未闭合的引号恢复,
        // 已有开引号时下一对才按闭引号输出。
        let (dq_open, sq_open) = quote_state(&self.context);
        self.dq_open = dq_open;
        self.sq_open = sq_open;
        // 光标跳走后,上一个词可能已经不在左邻;只有文本仍以它结尾才保留转移上下文。
        self.retain_last_words(text);
        // 光标左边是数字时,随后的半角句点更像小数点。
        self.after_digit = ends_with_digit(text);
        if self
            .last_commit
            .as_ref()
            .is_some_and(|anchor| !anchor.matches_tail(text))
        {
            self.last_commit = None;
            self.invalidate_attested_context();
        }
    }

    /// 左邻文本整体替换后,只保留仍在文本末尾的上一个/上上个词。
    pub(crate) fn retain_last_words(&mut self, text: &str) {
        if self
            .last_word
            .as_deref()
            .is_some_and(|word| !text.ends_with(word))
        {
            self.last_word = None;
        }
    }

    /// 清空已上屏上下文(焦点切到别处时调用)。
    pub fn clear_context(&mut self) {
        self.context.clear();
        self.break_word_context();
        self.last_pick = None;
        self.after_digit = false;
        // 新输入框的引号从开引号重新配对;沿用上一个文档的奇偶会把
        // 句首引号打成闭引号。
        self.dq_open = false;
        self.sq_open = false;
        self.invalidate_attested_context();
    }

    pub(crate) fn break_word_context(&mut self) {
        self.last_word = None;
        self.last_commit = None;
    }

    fn invalidate_attested_context(&mut self) {
        if !self.cands.iter().any(|cand| cand.attested_context) {
            return;
        }
        if self.comp.is_some() && self.page == 0 && self.hl == 0 {
            self.recompute();
        } else {
            // 不移动已经翻页或移动高亮的候选,只撤销对旧上下文的保护。
            for cand in &mut self.cands {
                cand.attested_context = false;
            }
            self.rev = self.rev.wrapping_add(1);
        }
    }

    pub(crate) fn push_context(&mut self, text: &str) {
        self.context.push_str(text);
        let excess = self.context.chars().count().saturating_sub(CONTEXT_KEEP);
        if excess > 0 {
            let cut = self
                .context
                .char_indices()
                .nth(excess)
                .map_or(self.context.len(), |(at, _)| at);
            self.context.replace_range(..cut, "");
        }
    }

    /// 把第 `index` 条候选移到首位并同步内联显示。
    ///
    /// 只换顺序,不增删候选;只有覆盖整个按键且没被用户跳过过的候选可以提升,
    /// 否则模型重排会把刚学到的负分立即抵消。成功返回 `true`。
    pub fn promote(&mut self, index: usize) -> bool {
        if index == 0 || index >= self.cands.len() {
            return false;
        }
        if self.cands[0].attested_context
            || !self.cands[index].full
            || self.cands[index].rejected
            || self.cands[index].quick
            || self.cands[index].no_learn
            || self.cands[index].weaker_reading
            || self.cands[index].needs_clause_prefix
        {
            return false;
        }
        // 停顿重排只移动整串候选,保留分段入口的槽位。
        if self.cands[0].full {
            let mut at = index;
            for earlier in (0..index).rev() {
                if self.cands[earlier].full {
                    self.cands.swap(earlier, at);
                    at = earlier;
                }
            }
        } else {
            let cand = self.cands.remove(index);
            self.cands.insert(0, cand);
        }
        if let Some(comp) = self.comp.as_mut()
            && let Some(top) = self.cands.first()
        {
            comp.seg_spans = top.spans.clone();
        }
        self.hl = 0;
        self.page = 0;
        self.rev = self.rev.wrapping_add(1);
        true
    }

    /// Reorder only the listed slots. Reject stale/invalid permutations before moving anything.
    pub fn reorder_candidates(&mut self, order: &[usize]) -> bool {
        if order.len() < 2 || order.is_sorted() || self.page != 0 || self.hl != 0 {
            return false;
        }
        if !order.iter().all(|&index| {
            self.cands.get(index).is_some_and(|c| {
                c.full
                    && !c.rejected
                    && !c.quick
                    && !c.no_learn
                    && !c.pinned
                    && !c.learned_enough()
                    && !c.attested_context
                    && !c.weaker_reading
                    && !c.needs_clause_prefix
            })
        }) {
            return false;
        }
        let mut slots = order.to_vec();
        slots.sort_unstable();
        if slots.windows(2).any(|pair| pair[0] == pair[1]) {
            return false;
        }
        let mut current = slots.clone();
        for (dest, &original) in order.iter().enumerate() {
            let source = current[dest..].iter().position(|&i| i == original).unwrap() + dest;
            self.cands.swap(slots[dest], slots[source]);
            current.swap(dest, source);
        }
        self.sync_spans();
        self.rev = self.rev.wrapping_add(1);
        true
    }

    pub fn highlight(&self) -> usize {
        self.hl
    }

    /// 移动高亮而不提交;宿主自绘候选时由 TSF 的 `SetSelection` 调用。
    /// 页号跟随高亮,内联预览与随后点选的内容保持一致。
    pub fn set_highlight(&mut self, index: usize) -> bool {
        if index >= self.cands.len() || index == self.hl {
            return false;
        }
        self.hl = index;
        self.page = index / self.cfg.page_size.max(1);
        self.sync_spans();
        true
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn page_size(&self) -> usize {
        self.cfg.page_size
    }

    /// 候选窗按内容自适应条数(横排放得下几条放几条);页大小跟随时,
    /// 数字键选中的与眼前看到的始终是同一条,翻页也不会跳过被挤掉的候选。
    /// 高亮不动,页号跟随高亮落到新页大小下的所在页。
    pub fn set_page_size(&mut self, size: usize) {
        let size = size.max(1);
        if self.cfg.page_size == size {
            return;
        }
        self.cfg.page_size = size;
        self.page = self.hl / size;
    }

    /// 上下翻一页;到边界返回 `false`。
    pub fn page_by(&mut self, delta: i32) -> bool {
        let ps = self.cfg.page_size.max(1);
        let pages = self.cands.len().div_ceil(ps);
        if pages <= 1 {
            return false;
        }
        let next = (self.page as i32 + delta).clamp(0, pages as i32 - 1) as usize;
        if next == self.page {
            return false;
        }
        self.page = next;
        self.hl = next * ps;
        self.sync_spans();
        true
    }

    /// 上下移动高亮;跨过页边界时页号一起走,候选窗始终有高亮项。
    pub(crate) fn move_highlight(&mut self, delta: i32) {
        if self.cands.is_empty() {
            return;
        }
        let last = self.cands.len() - 1;
        let next = (self.hl as i64 + i64::from(delta)).clamp(0, last as i64) as usize;
        if next == self.hl {
            return;
        }
        self.hl = next;
        self.page = next / self.cfg.page_size.max(1);
        self.sync_spans();
    }

    /// 高亮换候选后音节切分跟着换:隔音符判断与候选窗切分按当前高亮的边界。
    pub(crate) fn sync_spans(&mut self) {
        if let Some(cand) = self.cands.get(self.hl)
            && let Some(comp) = self.comp.as_mut()
        {
            comp.seg_spans = cand.spans.clone();
        }
    }

    /// 候选窗点选当前页第 `slot` 条(0 基);返回上屏事件。
    pub fn pick_in_page(&mut self, slot: usize) -> Vec<Event> {
        let ps = self.cfg.page_size.max(1);
        self.pick(self.page * ps + slot)
    }

    /// 置顶当前页第 `slot` 条:整个组合键下记住该词,此后直接放首位。
    /// 只收覆盖整串的普通候选;返回是否置顶成功。
    pub fn pin_in_page(&mut self, slot: usize) -> bool {
        let ps = self.cfg.page_size.max(1);
        self.pin(self.page * ps + slot)
    }

    pub fn pin(&mut self, index: usize) -> bool {
        let Some(cand) = self.cands.get(index).cloned() else {
            return false;
        };
        if cand.quick || (cand.no_learn && cand.action.is_none()) || !cand.full {
            return false;
        }
        let Some(comp) = self.comp.as_ref() else {
            return false;
        };
        // 键用方案展开后的规范串,并去掉隔音符:同一串字母的不同切分
        // (xian / xi'an)以及双拼与全拼都落到同一个置顶槽。
        let key = self.scheme.expand(&comp.raw).key.replace('\'', "");
        // 动作候选置顶写 `!命令 id`;重建时据此把它放回首位。
        if let Some(id) = cand.action.as_deref() {
            let pin = format!("!{id}");
            self.user
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .pin(&key, &pin);
            self.recompute();
            if let Some(pos) = self.cands.iter().position(|cand| cand.pinned) {
                self.hl = pos;
                self.page = pos / self.cfg.page_size.max(1);
                self.sync_spans();
            }
            return true;
        }
        let text = cand.learn_text.clone().unwrap_or_else(|| cand.text.clone());
        self.user
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .pin(&key, &text);
        self.recompute();
        // 置顶词保持高亮,后续按键接着补全。
        if let Some(pos) = self.cands.iter().position(|cand| cand.pinned) {
            self.hl = pos;
            self.page = pos / self.cfg.page_size.max(1);
            self.sync_spans();
        }
        true
    }

    /// 降权当前页第 `slot` 条:同键减弱一次记忆,让位给相邻候选。
    pub fn demote_in_page(&mut self, slot: usize) -> bool {
        let ps = self.cfg.page_size.max(1);
        self.demote(self.page * ps + slot)
    }

    pub fn demote(&mut self, index: usize) -> bool {
        let Some(cand) = self.cands.get(index).cloned() else {
            return false;
        };
        if cand.quick || cand.no_learn {
            return false;
        }
        let Some(comp) = self.comp.as_ref() else {
            return false;
        };
        let key = cand
            .learn_key
            .clone()
            .unwrap_or_else(|| self.scheme.key_for_spans(&comp.raw, &cand.spans));
        let text = cand.learn_text.clone().unwrap_or_else(|| cand.text.clone());
        self.user
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .skip(&key, &text);
        self.recompute();
        true
    }

    /// 宿主要求原样落定内联拼音原文(搜索框按 Enter):不补全剩余拼音,
    /// 也不改选候选。返回上屏文本。
    pub fn finalize_raw(&mut self) -> Option<String> {
        let mut text = self.commit_raw()?;
        if !self.modules.is_empty() {
            let ctx = module::CommitContext {
                host: &self.host,
                mode: self.mode,
            };
            self.modules.commit(&ctx, &mut text);
        }
        self.after_digit = ends_with_digit(&text);
        // 原样落定没有词边界信息,下一条候选不沿用整段显示文本作为前词。
        self.break_word_context();
        Some(text)
    }

    /// 按全局候选序号点选;宿主自绘候选时可能直接给绝对序号。
    pub fn pick(&mut self, index: usize) -> Vec<Event> {
        if index >= self.cands.len() {
            return Vec::new();
        }
        self.hl = index;
        // 点选动作候选执行命令,不上屏文字。
        let mut events = match self.take_action(index) {
            Some(id) => vec![Event::Action(id)],
            None => self
                .commit_highlighted()
                .map(|text| vec![Event::Commit(text)])
                .unwrap_or_default(),
        };
        self.apply_output(self.mode, &mut events);
        self.note_events(&events);
        events
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// 应用设置变更;组合中立刻按新配置重算候选。
    pub fn set_config(&mut self, cfg: Config) {
        self.cfg = cfg;
        if self.comp.is_some() {
            self.recompute();
        } else {
            self.page = 0;
            self.hl = 0;
        }
    }

    /// 宿主绑定表把当前整串输入映射到一个动作;不匹配时传 `None`。
    /// 动作只在整串完全匹配时作为候选出现,组合在时立即重算。
    pub fn set_action(&mut self, action: Option<Action>) {
        if self.action == action {
            return;
        }
        self.action = action;
        if self.comp.is_some() {
            self.recompute();
        }
    }

    pub fn export_user(&self) -> String {
        self.user.read().unwrap_or_else(|p| p.into_inner()).export()
    }

    pub fn import_user(&mut self, text: &str) {
        self.user
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .import(text);
    }

    /// 清空本机学习结果;设置页「重置个性化」与实验共用。
    pub fn reset_user(&self) {
        self.user.write().unwrap_or_else(|p| p.into_inner()).reset();
    }

    /// 模糊组的当前有效学习次数,设置页展示与 E2E 断言用。
    pub fn group_stats(&self) -> Vec<(&'static str, u32)> {
        self.user
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .group_stats()
    }
}

#[cfg(test)]
mod predict_tests {
    use super::*;
    use crate::Event;

    fn engine() -> Engine {
        let words = "今天\t9000\tjin'tian\n天气\t8000\ttian'qi\n很好\t7000\then'hao\n";
        let (dict, bad) = lueying_dict::Dictionary::from_text(words);
        assert_eq!(bad, 0);
        Engine::new(Arc::new(dict), Config::default())
    }

    fn commit(engine: &mut Engine, keys: &str) -> String {
        for c in keys.chars() {
            engine.process(Key::Letter(c));
        }
        engine
            .process(Key::Space)
            .into_iter()
            .find_map(|event| match event {
                Event::Commit(text) => Some(text),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn suggestions_are_available_right_after_commit() {
        let mut engine = engine();
        // 同一词序走两遍：转移证据达到联想门槛后，上屏「今天」立刻能给出「天气」。
        for _ in 0..2 {
            assert_eq!(commit(&mut engine, "jintian"), "今天");
            assert_eq!(commit(&mut engine, "tianqi"), "天气");
        }
        assert_eq!(commit(&mut engine, "jintian"), "今天");
        let suggestions = engine.predict_next(3);
        assert_eq!(
            suggestions.first().map(|word| word.text.as_str()),
            Some("天气")
        );
        assert_eq!(
            suggestions.first().and_then(|word| word.key.as_deref()),
            Some("tian'qi")
        );
    }
}
