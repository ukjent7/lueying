//! 拼音输入引擎:键状态机 + 组合编辑 + 候选管理。平台无关,上屏以事件表达。
//!
//! 默认行为对齐微软拼音习惯:空格上屏首选、数字选词、Enter 上屏原文、
//! Backspace 逐字母回删、Shift 切中英(组合中先上屏再切)、中文态标点转全角。

mod punct;
mod query;
mod quick;
mod segment;
mod typo;
mod user;

use std::sync::{Arc, RwLock};

use lueying_dict::Dictionary;
pub use lueying_dict::Fuzzy;
use lueying_dict::gram::Gram;

pub use query::Candidate;
pub use quick::CivilTime;
pub use user::UserDict;

/// 多个会话共享的用户词库;Server 让所有应用看到同一份学习结果。
pub type SharedUser = Arc<RwLock<UserDict>>;

/// 组合缓冲上限(字母与隔音符总数)。
/// 单次组合的原始输入上限。整句输入要能覆盖常见长句(约 26 个汉字);
/// 超过上限先落定当前组合,再用新按键起组,不能静默吞键。
pub const MAX_RAW: usize = 80;
/// 已上屏文本保留的字符数(重排与后续上下文模型使用)。
pub const CONTEXT_KEEP: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Chinese,
    English,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Letter(char),
    Digit(u8),
    Space,
    Enter,
    Esc,
    Backspace,
    Delete,
    /// Ctrl+Backspace:删除光标前一个音节。
    SegmentBackspace,
    /// Ctrl+Delete:删除光标后一个音节。
    SegmentDelete,
    Left,
    Right,
    /// Ctrl+Left/Ctrl+Right:按音节移动组合光标。
    SegmentLeft,
    SegmentRight,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// 删除当前高亮候选的学习记录(Shift+Delete);词典词条本身不受影响。
    DeleteCandidate,
    Punct(char),
    Shift,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// 上屏一段文本。
    Commit(String),
    /// 模式切换。
    Mode(Mode),
    /// 键不被输入法处理,交还宿主直通。
    Passthrough(Key),
}

#[derive(Clone, Debug)]
pub struct Config {
    pub fuzzy: Fuzzy,
    pub chinese_punct: bool,
    pub page_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 零配置:核心模糊组默认参与召回,排序由处数罚分与学习覆盖负责。
            fuzzy: Fuzzy::auto(),
            chinese_punct: true,
            page_size: 5,
        }
    }
}

/// 组合态快照(UI 用)。
#[derive(Clone, Debug)]
pub struct Composition {
    /// 原始输入(小写字母与隔音符)。
    pub raw: String,
    /// 光标(原始串内字节偏移,内容全 ASCII)。
    pub cursor: usize,
    /// 内联显示文本:首选转换,无候选时原文。
    pub display: String,
    /// 当前切分各音节的原始区间。
    pub seg_spans: Vec<(usize, usize)>,
}

pub struct Engine {
    /// 词典跨会话共享(Server 进程里多个宿主会话共用一份)。
    dict: std::sync::Arc<Dictionary>,
    /// 可选的 rime `.gram` 语言模型;缺失或损坏时整句退化为纯词频。
    gram: Option<std::sync::Arc<Gram>>,
    /// 跨按键复用的搭配分缓存;键是 (前文,词,句末)。
    gram_cache: std::collections::HashMap<(String, String, bool), f64>,
    user: SharedUser,
    /// 私密输入范围里关闭学习;输入本身不受影响。
    learning: bool,
    cfg: Config,
    mode: Mode,
    /// 全角模式:英文态与中文态的空闲数字/空格/未映射标点按全角上屏。
    fullwidth: bool,
    comp: Option<Composition>,
    /// 当前组合里已经上屏过分段词(这时左邻文本不含这些词)。
    comp_committed: bool,
    /// 同一次组合里连续上屏的片段,凑成整串自动造词。
    phrase: Option<(String, String)>,
    cands: Vec<query::Candidate>,
    hl: usize,
    page: usize,
    dq_open: bool,
    sq_open: bool,
    /// 刚刚输入/上屏的是 ASCII 数字;中文模式下数字后的 `.` 保持半角。
    after_digit: bool,
    /// 最近上屏的文本,最多 [`CONTEXT_KEEP`] 字。焦点切换时清空。
    context: String,
    /// 分段上屏已在重算前写入上下文的提交数;`note_events` 据此跳过重复学习。
    context_handled: usize,
    /// 快捷候选上屏的条数;`note_events` 据此跳过字级搭配学习,但仍写入左邻文本。
    quick_handled: usize,
    /// 最近上屏的最后一个词;词级转移加分与学习使用。
    last_word: Option<String>,
    /// 候选提交已在 `apply_consume` 学过词级转移的条数。
    transition_handled: usize,
    /// 宿主注入的本地时间;快捷输入按它生成日期/时间候选。
    now: Option<CivilTime>,
    /// 候选集合每次变化自增;异步重排据此丢弃过期结果。
    rev: u64,
}

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
            transition_handled: 0,
            now: None,
            rev: 0,
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
            // 私密/普通字段切换:前一段文本、引号配对和词序都不能带进新字段。
            self.clear_context();
            self.phrase = None;
        }
        self.learning = enabled;
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

    /// 最近上屏的文本(最多 [`CONTEXT_KEEP`] 字)。
    pub fn context(&self) -> &str {
        &self.context
    }

    /// 用光标前文本替换上下文,只保留末尾 [`CONTEXT_KEEP`] 字。
    ///
    /// TSF 在组合开始时上报左邻文本;它包含之前所有已上屏内容,因此可以整体替换。
    pub fn set_context(&mut self, text: &str) {
        self.context.clear();
        self.push_context(text);
        // 光标跳走后,上一个词可能已经不在左邻;只有文本仍以它结尾才保留转移上下文。
        if self
            .last_word
            .as_deref()
            .is_some_and(|word| !text.ends_with(word))
        {
            self.last_word = None;
        }
        // 光标左边是数字时,随后的半角句点更像小数点。
        self.after_digit = ends_with_digit(text);
    }

    /// 清空已上屏上下文(焦点切到别处时调用)。
    pub fn clear_context(&mut self) {
        self.context.clear();
        self.last_word = None;
        self.after_digit = false;
        // 新输入框的引号从开引号重新配对;沿用上一个文档的奇偶会把
        // 句首引号打成闭引号。
        self.dq_open = false;
        self.sq_open = false;
    }

    fn push_context(&mut self, text: &str) {
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
    /// 只换顺序,不增删候选;只有覆盖整个按键的候选可以提升。成功返回 `true`。
    pub fn promote(&mut self, index: usize) -> bool {
        if index == 0 || index >= self.cands.len() || !self.cands[index].full {
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
    fn has_pages(&self) -> bool {
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
    fn move_highlight(&mut self, delta: i32) {
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
    fn sync_display(&mut self) {
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
        let text = if comp.display.is_empty() {
            comp.raw
        } else {
            comp.display
        };
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
        Some(text)
    }

    /// 忘掉高亮候选的学习记录;候选仍可由主词典提供,只是回到原始词频。
    /// 返回是否真的删掉了一条用户词。
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
        let events = self
            .commit_highlighted()
            .map(|text| vec![Event::Commit(text)])
            .unwrap_or_default();
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

    /// 处理一次按键,返回由此产生的全部事件。
    pub fn process(&mut self, key: Key) -> Vec<Event> {
        let events = self.process_inner(key);
        self.note_events(&events);
        events
    }

    /// 把整个组合按当前切分逐段落定(失焦、别的窗口切全局模式)。
    /// 与空格不同,这里不会只上屏吃前缀的候选而把剩余拼音留在组合里。
    pub fn flush(&mut self) -> Vec<Event> {
        let events = self.commit_all();
        self.note_events(&events);
        events
    }

    /// 上屏文本进入左邻上下文(ASCII 直通内容不算),同时记住最后一个字符
    /// 是不是数字。直通的数字/字母/标点也在这里更新,后续句号才能判断
    /// 该出半角小数点还是中文句号。
    fn note_events(&mut self, events: &[Event]) {
        for event in events {
            match event {
                Event::Commit(text) => {
                    if self.context_handled > 0 {
                        self.context_handled -= 1;
                    } else if !text.is_ascii() {
                        // 快捷输入是系统生成的数字/日期,不进个人搭配;左邻文本
                        // 仍要更新,让紧随其后的候选看到正确的上下文。
                        if self.learning && self.quick_handled == 0 {
                            self.user
                                .write()
                                .unwrap_or_else(|p| p.into_inner())
                                .learn_context(&self.context, text);
                        }
                        self.push_context(text);
                    }
                    // 候选提交已在 apply_consume 学过词级转移;标点、英文和
                    // 原样上屏会切断词序,下一条候选不再沿用上一个词。
                    if self.transition_handled > 0 {
                        self.transition_handled -= 1;
                    } else {
                        self.last_word = None;
                    }
                    if self.quick_handled > 0 {
                        self.quick_handled -= 1;
                    }
                    self.after_digit = ends_with_digit(text);
                }
                Event::Passthrough(Key::Digit(_)) => {
                    self.after_digit = true;
                    // 直通内容由宿主直接写进文档,已经断开中文词序。
                    self.last_word = None;
                }
                Event::Passthrough(_) => {
                    self.after_digit = false;
                    self.last_word = None;
                }
                Event::Mode(_) => {}
            }
        }
    }

    fn process_inner(&mut self, key: Key) -> Vec<Event> {
        if key == Key::Shift {
            // 首选可能只吃前缀音节;切英文前把整串都落定,不能留下半个组合。
            let mut events = if self.mode == Mode::Chinese {
                self.commit_all()
            } else {
                Vec::new()
            };
            self.mode = if self.mode == Mode::Chinese {
                Mode::English
            } else {
                Mode::Chinese
            };
            events.push(Event::Mode(self.mode));
            return events;
        }
        if self.mode == Mode::English {
            return self.english_events(key);
        }
        if self.comp.is_some() {
            self.process_composing(key)
        } else {
            self.process_idle(key)
        }
    }

    /// 英文态:全角开着时把可打印 ASCII 按键转成全角上屏,其余键照常直通。
    fn english_events(&mut self, key: Key) -> Vec<Event> {
        if !self.fullwidth {
            return vec![Event::Passthrough(key)];
        }
        let text = match key {
            Key::Letter(c) => punct::fullwidth(c),
            Key::Digit(d) => char::from_digit(u32::from(d), 10).and_then(punct::fullwidth),
            Key::Punct(p) => punct::fullwidth(p),
            Key::Space => Some('\u{3000}'),
            _ => None,
        };
        match text {
            Some(c) => vec![Event::Commit(c.to_string())],
            None => vec![Event::Passthrough(key)],
        }
    }

    fn process_idle(&mut self, key: Key) -> Vec<Event> {
        match key {
            Key::Letter(c) if c.is_ascii_lowercase() => {
                self.begin_composition(c);
                Vec::new()
            }
            // 全角下中文态直接上大写字母、数字与空格;小写字母仍起拼音。
            Key::Letter(c) if self.fullwidth => {
                let text = punct::fullwidth(c).map_or_else(|| c.to_string(), String::from);
                vec![Event::Commit(text)]
            }
            Key::Digit(d) if self.fullwidth => {
                let text = char::from_digit(u32::from(d), 10)
                    .and_then(punct::fullwidth)
                    .map_or_else(|| d.to_string(), String::from);
                vec![Event::Commit(text)]
            }
            Key::Space if self.fullwidth => vec![Event::Commit('\u{3000}'.to_string())],
            Key::Punct(p) => self.punct_events(p),
            _ => vec![Event::Passthrough(key)],
        }
    }

    fn begin_composition(&mut self, c: char) {
        self.comp = Some(Composition {
            raw: c.to_string(),
            cursor: 1,
            display: String::new(),
            seg_spans: Vec::new(),
        });
        self.comp_committed = false;
        self.recompute();
    }

    fn process_composing(&mut self, key: Key) -> Vec<Event> {
        match key {
            Key::Letter(c) if c.is_ascii_lowercase() => {
                if self.comp.as_ref().is_some_and(|comp| comp.raw.len() >= MAX_RAW) {
                    // 组合到达上限:先把当前输入按首选落定,再用这一键起新组合。
                    // 直接丢弃会让长句末尾的按键无声消失,文档与用户预期错位。
                    let mut events = self.commit_all();
                    self.phrase = None;
                    self.begin_composition(c);
                    return events;
                }
                if let Some(comp) = self.comp.as_mut() {
                    comp.raw.insert(comp.cursor, c);
                    comp.cursor += 1;
                }
                self.recompute();
                Vec::new()
            }
            Key::Punct('\'') => {
                self.try_insert_apostrophe();
                Vec::new()
            }
            Key::Space => self
                .commit_highlighted()
                .map(|t| vec![Event::Commit(t)])
                .unwrap_or_default(),
            Key::Enter => {
                let raw = self.comp.take().map(|c| c.raw).unwrap_or_default();
                self.comp_committed = false;
                self.phrase = None;
                self.cands.clear();
                self.rev = self.rev.wrapping_add(1);
                vec![Event::Commit(raw)]
            }
            Key::Esc => {
                self.clear_comp();
                Vec::new()
            }
            Key::Backspace => {
                self.delete_char(true);
                Vec::new()
            }
            Key::Delete => {
                self.delete_char(false);
                Vec::new()
            }
            Key::SegmentBackspace => {
                self.delete_segment(true);
                Vec::new()
            }
            Key::SegmentDelete => {
                self.delete_segment(false);
                Vec::new()
            }
            Key::SegmentLeft => {
                self.move_segment(-1);
                Vec::new()
            }
            Key::SegmentRight => {
                self.move_segment(1);
                Vec::new()
            }
            Key::DeleteCandidate => {
                self.forget_highlighted();
                Vec::new()
            }
            Key::Left => {
                self.move_cursor_char(-1);
                Vec::new()
            }
            Key::Right => {
                self.move_cursor_char(1);
                Vec::new()
            }
            Key::Home => {
                if let Some(comp) = self.comp.as_mut() {
                    comp.cursor = 0;
                }
                Vec::new()
            }
            Key::End => {
                if let Some(comp) = self.comp.as_mut() {
                    comp.cursor = comp.raw.len();
                }
                Vec::new()
            }
            Key::Up => {
                self.move_highlight(-1);
                Vec::new()
            }
            Key::Down => {
                self.move_highlight(1);
                Vec::new()
            }
            Key::PageUp | Key::PageDown => {
                self.page_by(if key == Key::PageUp { -1 } else { 1 });
                Vec::new()
            }
            // v 模式的第一位小数点属于数字;已有小数点时恢复普通标点。
            Key::Punct('.') if self.number_dot_mode() => {
                if self.append_number_dot() {
                    Vec::new()
                } else {
                    let mut events = self.commit_all();
                    self.phrase = None;
                    events.push(Event::Commit(".".to_string()));
                    events
                }
            }
            // 微软拼音习惯:组合中 -/= 翻页;只有一页时退回标点。
            Key::Punct('-') if self.has_pages() => {
                self.page_by(-1);
                Vec::new()
            }
            Key::Punct('=') if self.has_pages() => {
                self.page_by(1);
                Vec::new()
            }
            // v 模式里数字是数字的一部分,不再当候选序号;上屏仍用空格/回车。
            Key::Digit(d) if self.number_mode() => self.number_digit(d),
            Key::Digit(d) => self.select_digit(d),
            Key::Punct(p) => {
                // 标点把句子切开了,整串落定后再上屏标点;已选片段不再拼成整句词条。
                let mut events = self.commit_all();
                self.phrase = None;
                // 刚上屏的是中文候选;组句前的数字不再影响这个句号。
                self.after_digit = false;
                // 没有中文映射(或关了中文标点)时按 ASCII 上屏;
                // 直通会让应用在组句未收时插入字符,文档与引擎就错位了。
                let mapped = if self.cfg.chinese_punct {
                    punct::map(p, &mut self.dq_open, &mut self.sq_open, &mut self.after_digit)
                } else {
                    None
                };
                let text = mapped.map_or_else(
                    || {
                        if self.fullwidth {
                            punct::fullwidth(p)
                                .map_or_else(|| p.to_string(), String::from)
                        } else {
                            p.to_string()
                        }
                    },
                    |s| s.to_string(),
                );
                events.push(Event::Commit(text));
                events
            }
            // 大写字母(Shift+字母)在这里落定整串组合,再由调用方写进文档;
            // 返回直通会让应用在组合未收时插字,留下无法收拾的内联拼音。
            Key::Letter(c) => {
                let mut events = self.commit_all();
                self.phrase = None;
                let text = if self.fullwidth {
                    punct::fullwidth(c).map_or_else(|| c.to_string(), String::from)
                } else {
                    c.to_string()
                };
                events.push(Event::Commit(text));
                events
            }
            // Shift 在 process 入口已处理,组合态里不会再遇到。
            Key::Shift => Vec::new(),
        }
    }

    /// 标点:开中文标点则上屏全角,否则全角模式下上屏全角 ASCII,再否则直通。
    fn punct_events(&mut self, p: char) -> Vec<Event> {
        if self.cfg.chinese_punct {
            if let Some(s) =
                punct::map(p, &mut self.dq_open, &mut self.sq_open, &mut self.after_digit)
            {
                return vec![Event::Commit(s.to_string())];
            }
        }
        if self.fullwidth
            && let Some(c) = punct::fullwidth(p)
        {
            return vec![Event::Commit(c.to_string())];
        }
        vec![Event::Passthrough(Key::Punct(p))]
    }

    fn select_digit(&mut self, d: u8) -> Vec<Event> {
        let ps = self.cfg.page_size.max(1);
        let start = self.page * ps;
        let end = (start + ps).min(self.cands.len());
        if d >= 1 && d as usize <= end - start {
            let idx = start + d as usize - 1;
            match self.apply_consume(idx) {
                Some(t) => vec![Event::Commit(t)],
                None => vec![Event::Passthrough(Key::Digit(d))],
            }
        } else {
            // 当前页没有这个序号:先收组句再上屏数字。组合中直通会让应用
            // 把数字插进内联拼音里,引擎与文档随即错位。
            let mut events = self.commit_all();
            self.phrase = None;
            events.push(Event::Commit(d.to_string()));
            events
        }
    }

    /// 组合是否处于 v 数字模式:字母 v 后只跟数字与至多一个小数点。
    fn number_mode(&self) -> bool {
        self.comp.as_ref().is_some_and(|comp| {
            comp.raw
                .strip_prefix('v')
                .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
        })
    }

    /// 第一位小数点进入数字;已有小数点或还没有数字时按普通标点走。
    fn number_dot_mode(&self) -> bool {
        self.comp.as_ref().is_some_and(|comp| {
            comp.raw.strip_prefix('v').is_some_and(|rest| {
                !rest.is_empty() && !rest.contains('.') && rest.bytes().all(|b| b.is_ascii_digit())
            })
        })
    }

    /// v 模式数字键进入组合;组合已满时退回候选序号,不静默吞键。
    fn number_digit(&mut self, d: u8) -> Vec<Event> {
        if self.comp.as_ref().is_some_and(|comp| comp.raw.len() >= MAX_RAW) {
            return self.select_digit(d);
        }
        let Some(c) = char::from_digit(u32::from(d), 10) else {
            return self.select_digit(d);
        };
        if let Some(comp) = self.comp.as_mut() {
            comp.raw.insert(comp.cursor, c);
            comp.cursor += 1;
        }
        self.recompute();
        Vec::new()
    }

    /// 把第一位小数点写进 v 数字;组合已满时返回 `false`,由调用方落定。
    fn append_number_dot(&mut self) -> bool {
        let inserted = {
            let Some(comp) = self.comp.as_mut() else {
                return false;
            };
            if comp.raw.len() >= MAX_RAW {
                false
            } else {
                comp.raw.insert(comp.cursor, '.');
                comp.cursor += 1;
                true
            }
        };
        if inserted {
            self.recompute();
        }
        inserted
    }

    /// 逐段落定整个组合;没有候选时上屏原文。返回按序的上屏事件。
    fn commit_all(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(text) = self.commit_highlighted() {
            events.push(Event::Commit(text));
        }
        events
    }

    /// 上屏高亮候选;无候选时上屏原文。
    fn commit_highlighted(&mut self) -> Option<String> {
        self.comp.as_ref()?;
        if self.cands.is_empty() {
            let raw = self.comp.take().map(|c| c.raw);
            self.comp_committed = false;
            self.phrase = None;
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
            return raw;
        }
        self.apply_consume(self.hl.min(self.cands.len() - 1))
    }

    /// 应用第 idx 个候选:上屏文本,吃掉 consume 个音节,剩余留作组合并学习。
    fn apply_consume(&mut self, idx: usize) -> Option<String> {
        let cand = self.cands.get(idx)?.clone();
        let cut = cand.spans.get(cand.consume - 1).map(|s| s.1)?;
        let raw = self.comp.as_ref()?.raw.clone();
        // 快捷候选不是词:不写用户词库、不学组级适应,也不把日期/金额
        // 当作用词接进词级转移。它们也不参与同一次组合的自动造词。
        if cand.quick {
            self.phrase = None;
            self.quick_handled += 1;
        }
        // 用户词库的键必须和查询时的音节键一致(ni'hao),不能存原始串(nihao),
        // 否则多音节词学完也匹配不上。
        let learned = if cand.quick {
            String::new()
        } else {
            cand.learn_key.clone().unwrap_or_else(|| {
                cand.spans
                    .iter()
                    .take(cand.consume)
                    .map(|&(s, e)| lueying_dict::syllables::canonical(&raw[s..e]))
                    .collect::<Vec<_>>()
                    .join("'")
            })
        };
        if self.learning && !cand.quick {
            let mut user = self.user.write().unwrap_or_else(|p| p.into_inner());
            user.learn(&learned, &cand.text);
            if cand.groups != 0 {
                user.learn_groups(cand.groups);
            }
            user.learn_transitions(self.last_word.as_deref(), &cand.words);
        }
        if cand.quick {
            // 让 `note_events` 把上一条词序切断,而不是把日期记成前词。
            self.last_word = None;
        } else {
            self.last_word = cand.words.last().cloned();
            self.transition_handled += 1;
        }
        let rest = raw[cut..].trim_start_matches('\'').to_string();
        // 同一次组合里连续选词:把各段拼音与文本拼起来,整串结束时造一个
        // 整句词条。只选一次(整串或单段)时上面已经学过,不重复记。
        let phrase = if self.learning && !cand.quick {
            match self.phrase.take() {
                Some((mut key, mut text)) => {
                    key.push('\'');
                    key.push_str(&learned);
                    text.push_str(&cand.text);
                    Some((key, text))
                }
                None if !rest.is_empty() => Some((learned, cand.text.clone())),
                None => None,
            }
        } else {
            None
        };
        if rest.is_empty() {
            if let Some((key, text)) = phrase
                && text.chars().count() > 1
            {
                self.user
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .learn(&key, &text);
            }
        } else {
            self.phrase = phrase;
        }
        if rest.is_empty() {
            self.comp = None;
            self.comp_committed = false;
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
        } else {
            // 剩余拼音马上要重算:先把刚上屏的片段并入左邻上下文,下一段的
            // 候选才能看到刚形成的搭配。`note_events` 会跳过这次重复处理。
            if !cand.text.is_ascii() {
                if self.learning {
                    self.user
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .learn_context(&self.context, &cand.text);
                }
                self.push_context(&cand.text);
            }
            self.context_handled += 1;
            let comp = self.comp.as_mut()?;
            comp.raw = rest;
            // 选掉前缀后光标仍在剩余拼音末尾,继续按键才会接在后面;
            // 置 0 会让下一个字母插到最前面,变成 `htianqi`。
            comp.cursor = comp.raw.len();
            self.comp_committed = true;
            self.recompute();
        }
        Some(cand.text)
    }

    fn try_insert_apostrophe(&mut self) {
        let ok = if let Some(comp) = self.comp.as_ref() {
            comp.seg_spans.iter().rev().any(|&(s, e)| {
                e == comp.cursor && lueying_dict::syllables::is_syllable(&comp.raw[s..e])
            })
        } else {
            false
        };
        if ok
            && let Some(comp) = self.comp.as_mut()
            && comp.raw.len() < MAX_RAW
        {
            comp.raw.insert(comp.cursor, '\'');
            comp.cursor += 1;
            self.recompute();
        }
    }

    /// 逐字母回删/前删;与微软拼音、微信输入法一致,不按音节整段删。
    fn delete_char(&mut self, left: bool) {
        let Some(comp) = self.comp.as_mut() else {
            return;
        };
        if left {
            if comp.cursor == 0 {
                return;
            }
            let at = comp.cursor - 1;
            comp.raw.remove(at);
            comp.cursor = at;
        } else {
            if comp.cursor >= comp.raw.len() {
                return;
            }
            comp.raw.remove(comp.cursor);
        }
        if comp.raw.is_empty() {
            self.comp = None;
            self.comp_committed = false;
            self.phrase = None;
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
        } else {
            self.recompute();
        }
    }

    /// 光标左右移动一个字符。
    fn move_cursor_char(&mut self, dir: i32) {
        let Some(comp) = self.comp.as_mut() else {
            return;
        };
        comp.cursor = if dir < 0 {
            comp.cursor.saturating_sub(1)
        } else {
            (comp.cursor + 1).min(comp.raw.len())
        };
    }

    /// Ctrl+Left/Ctrl+Right:按音节移动光标。音节边界取音节图的最长匹配路径,
    /// 与候选无关;隔音符只当分隔,不占音节。
    fn move_segment(&mut self, dir: i32) {
        let Some(comp) = self.comp.as_ref() else {
            return;
        };
        let spans = segment::first_path(&comp.raw);
        let bytes = comp.raw.as_bytes();
        let cursor = comp.cursor;
        let next = if dir < 0 {
            let mut at = cursor;
            while at > 0 && bytes[at - 1] == b'\'' {
                at -= 1;
            }
            spans
                .iter()
                .rev()
                .find(|span| span.0 < at && at <= span.1)
                .map_or(0, |span| span.0)
        } else {
            let mut at = cursor;
            while at < bytes.len() && bytes[at] == b'\'' {
                at += 1;
            }
            spans
                .iter()
                .find(|span| span.0 <= at && at < span.1)
                .map_or(bytes.len(), |span| span.1)
        };
        if let Some(comp) = self.comp.as_mut() {
            comp.cursor = next;
        }
    }

    /// Ctrl+Backspace/Ctrl+Delete:删除光标前/后一个音节,连同相邻的一个隔音符。
    fn delete_segment(&mut self, left: bool) {
        let Some(comp) = self.comp.as_ref() else {
            return;
        };
        let mut raw = comp.raw.clone();
        let cursor = comp.cursor;
        let spans = segment::first_path(&raw);
        let bytes = raw.as_bytes();
        let (mut start, mut end) = if left {
            let mut at = cursor;
            while at > 0 && bytes[at - 1] == b'\'' {
                at -= 1;
            }
            let Some(span) = spans.iter().rev().find(|span| span.0 < at && at <= span.1) else {
                return;
            };
            (span.0, cursor)
        } else {
            let mut at = cursor;
            while at < bytes.len() && bytes[at] == b'\'' {
                at += 1;
            }
            let Some(span) = spans.iter().find(|span| span.0 <= at && at < span.1) else {
                return;
            };
            (cursor, span.1)
        };
        // 删掉音节时连同它一侧的隔音符一起收走,避免留下孤立的 `'`。
        if start > 0 && bytes[start - 1] == b'\'' {
            start -= 1;
        } else if end < bytes.len() && bytes[end] == b'\'' {
            end += 1;
        }
        if start >= end {
            return;
        }
        raw.replace_range(start..end, "");
        if raw.is_empty() {
            self.clear_comp();
            return;
        }
        if let Some(comp) = self.comp.as_mut() {
            comp.raw = raw;
            comp.cursor = start.min(comp.raw.len());
        }
        self.recompute();
    }

    fn clear_comp(&mut self) {
        self.comp = None;
        self.comp_committed = false;
        self.phrase = None;
        self.cands.clear();
        self.hl = 0;
        self.page = 0;
        self.rev = self.rev.wrapping_add(1);
    }

    /// 重算候选与内联显示。任何组合变化后必须调用。
    fn recompute(&mut self) {
        self.rev = self.rev.wrapping_add(1);
        self.hl = 0;
        self.page = 0;
        let Some(comp) = self.comp.as_mut() else {
            self.cands.clear();
            return;
        };
        // 缓存只增不减会随输入历史膨胀;超过上限就整体丢弃,后续按键会重新填充。
        if self.gram_cache.len() > 4096 {
            self.gram_cache.clear();
        }
        let context = self.context.clone();
        let user = self.user.read().unwrap_or_else(|p| p.into_inner());
        let mut built = query::build(
            &self.dict,
            &user,
            &self.cfg.fuzzy,
            self.gram.as_deref(),
            &mut self.gram_cache,
            &context,
            self.last_word.as_deref(),
            &comp.raw,
        );
        let now = self.now.unwrap_or_else(CivilTime::now_utc);
        // 日期/时间/金额排在「可信的整词」之后,但越过未打完的前缀和
        // 简拼硬拼出来的整句:`sj` 仍以「时间」为首选,`rq`/`xq` 直接出快捷项。
        let quick = quick::candidates(&comp.raw, now);
        if !quick.is_empty() {
            built.cands.retain(|cand| !quick.iter().any(|q| q.text == cand.text));
            let keep_first = built.cands.first().is_some_and(|cand| {
                cand.full && !cand.sentence && (cand.trusted || cand.user_freq > 0)
            });
            let position = if keep_first { 1 } else { 0 };
            for cand in quick.into_iter().rev() {
                built.cands.insert(position, cand);
            }
            // 去重可能删掉原来的首选;内联显示始终跟随插入后的第一条。
            if let Some(top) = built.cands.first() {
                built.display = top.text.clone();
            }
        } else if let Some((at, quick)) = quick::keyword(&built.cands, &comp.raw, now) {
            // 全拼打出了「日期/时间/星期」:写法紧跟这个词,不抢它的首选。
            for cand in quick.into_iter().rev() {
                built.cands.insert(at + 1, cand);
            }
        }
        comp.display = built.display;
        comp.seg_spans = built
            .cands
            .first()
            .map(|c| c.spans.clone())
            .unwrap_or_else(|| segment::first_path(&comp.raw));
        self.cands = built.cands;
    }
}

/// 文本末尾是不是数字;全角数字也按数字处理,小数点和 IP 地址不必切半角。
fn ends_with_digit(text: &str) -> bool {
    text.chars()
        .last()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, '０'..='９'))
}
