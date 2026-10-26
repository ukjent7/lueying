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
            gram_cache: std::collections::HashMap::new(),
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
            second_last_word: None,
            host: String::new(),
            transition_handled: 0,
            now: None,
            rev: 0,
            modules: Arc::new(module::Modules::new()),
        }
    }

    /// 挂上宿主组合好的功能模块(链式构造)。
    pub fn with_modules(mut self, modules: Arc<module::Modules>) -> Self {
        self.modules = modules;
        self
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
            // 私密/普通字段切换:前一段文本、引号配对和词序都不能带进新字段。
            self.clear_context();
            self.phrase = None;
        }
        self.learning = enabled;
    }

    /// 记录宿主 exe 名,用于按应用学习;空名(未知宿主)不产生应用偏好。
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
    }

    /// 左邻文本整体替换后,只保留仍在文本末尾的上一个/上上个词。
    pub(crate) fn retain_last_words(&mut self, text: &str) {
        let Some(word) = self.last_word.clone() else {
            self.second_last_word = None;
            return;
        };
        if !text.ends_with(&word) {
            self.last_word = None;
            self.second_last_word = None;
            return;
        }
        let before = &text[..text.len() - word.len()];
        if self
            .second_last_word
            .as_deref()
            .is_some_and(|previous| !before.ends_with(previous))
        {
            self.second_last_word = None;
        }
    }

    /// 清空已上屏上下文(焦点切到别处时调用)。
    pub fn clear_context(&mut self) {
        self.context.clear();
        self.last_word = None;
        self.second_last_word = None;
        self.after_digit = false;
        // 新输入框的引号从开引号重新配对;沿用上一个文档的奇偶会把
        // 句首引号打成闭引号。
        self.dq_open = false;
        self.sq_open = false;
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
        if index == 0
            || index >= self.cands.len()
            || !self.cands[index].full
            || self.cands[index].rejected
        {
            return false;
        }
        let cand = self.cands.remove(index);
        self.cands.insert(0, cand);
        if let Some(comp) = self.comp.as_mut()
            && let Some(top) = self.cands.first()
        {
            comp.display = top.text.clone();
            comp.seg_spans = top.spans.clone();
        }
        self.hl = 0;
        self.page = 0;
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
        self.sync_display();
        true
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn page_size(&self) -> usize {
        self.cfg.page_size
    }

    /// 候选是否不止一页(`-`/`=` 只在多页时翻页)。
    pub(crate) fn has_pages(&self) -> bool {
        let ps = self.cfg.page_size.max(1);
        self.cands.len().div_ceil(ps) > 1
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
        self.sync_display();
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
        self.sync_display();
    }

    /// 内联文本跟随高亮:方向键换候选后,未上屏的显示与即将上屏的内容一致。
    pub(crate) fn sync_display(&mut self) {
        if let Some(cand) = self.cands.get(self.hl)
            && let Some(comp) = self.comp.as_mut()
        {
            comp.display = cand.text.clone();
            comp.seg_spans = cand.spans.clone();
        }
    }

    /// 候选窗点选当前页第 `slot` 条(0 基);返回上屏事件。
    pub fn pick_in_page(&mut self, slot: usize) -> Vec<Event> {
        let ps = self.cfg.page_size.max(1);
        self.pick(self.page * ps + slot)
    }

    /// 宿主要求原样落定当前显示文本(搜索框按 Enter):上屏内联预览,
    /// 不补全剩余拼音,也不改选下一条候选。返回上屏文本。
    pub fn finalize_display(&mut self) -> Option<String> {
        let comp = self.comp.take()?;
        let quick = self.cands.get(self.hl).is_some_and(|cand| cand.quick);
        let mut text = if comp.display.is_empty() {
            comp.raw
        } else {
            comp.display
        };
        if !self.modules.is_empty() {
            let ctx = module::CommitContext {
                host: &self.host,
                mode: self.mode,
            };
            self.modules.commit(&ctx, &mut text);
        }
        self.comp_committed = false;
        self.phrase = None;
        self.cands.clear();
        self.hl = 0;
        self.page = 0;
        self.rev = self.rev.wrapping_add(1);
        if !text.is_ascii() {
            // 快捷文本不学个人搭配,但仍作为后续候选的左邻上下文。
            if self.learning && !quick {
                self.user
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .learn_context(&self.context, &text);
            }
            self.push_context(&text);
        }
        self.after_digit = ends_with_digit(&text);
        // 原样落定没有词边界信息,下一条候选不沿用整段显示文本作为前词。
        self.last_word = None;
        self.second_last_word = None;
        Some(text)
    }

    /// 忘掉高亮候选的学习记录(用户词与跳过负分);候选仍可由主词典提供,
    /// 只是回到原始词频。返回是否真的删掉了一条记录。
    pub fn forget_highlighted(&mut self) -> bool {
        let Some(cand) = self.cands.get(self.hl).cloned() else {
            return false;
        };
        // 日期/金额不是用户词,没有可忘的学习记录;也不能按同文删到真正的词。
        if cand.quick {
            return false;
        }
        // 用户可能用全拼学过、现在用简拼忘掉,按词删除所有学习键下的记录。
        let removed = self
            .user
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove_word(&cand.text);
        if removed {
            self.recompute();
            // 主词典仍能给出这个词时保持它高亮,连按 Shift+Delete 不会误删下一条。
            if let Some(index) = self.cands.iter().position(|c| c.text == cand.text) {
                self.hl = index;
                self.page = index / self.cfg.page_size.max(1);
                self.sync_display();
            }
        }
        removed
    }

    /// 按全局候选序号点选;宿主自绘候选时可能直接给绝对序号。
    pub fn pick(&mut self, index: usize) -> Vec<Event> {
        if index >= self.cands.len() {
            return Vec::new();
        }
        self.hl = index;
        let mut events = self
            .commit_highlighted()
            .map(|text| vec![Event::Commit(text)])
            .unwrap_or_default();
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

    pub fn export_user(&self) -> String {
        self.user.read().unwrap_or_else(|p| p.into_inner()).export()
    }

    pub fn import_user(&mut self, text: &str) {
        self.user
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .import(text);
    }

    /// 清空所有会话共用的用户词库。
    pub fn clear_user(&mut self) {
        self.user.write().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// 模糊组的当前有效学习次数,设置页展示与 E2E 断言用。
    pub fn group_stats(&self) -> Vec<(&'static str, u32)> {
        self.user
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .group_stats()
    }

    /// 只清空组级适应,保留词条。
    pub fn reset_groups(&mut self) {
        self.user
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .reset_groups();
    }
}
