//! 拼音输入引擎:键状态机 + 组合编辑 + 候选管理。平台无关,上屏以事件表达。
//!
//! 默认行为对齐微软拼音习惯:空格上屏首选、数字选词、Enter 上屏原文、
//! Backspace 按音节回删、Shift 切中英(组合中先上屏再切)、中文态标点转全角。

mod punct;
mod query;
mod segment;
mod user;

use std::sync::{Arc, RwLock};

use pinyin_dict::gram::Gram;
use pinyin_dict::{Dictionary, Fuzzy};

pub use query::Candidate;
pub use user::UserDict;

/// 多个会话共享的用户词库;Server 让所有应用看到同一份学习结果。
pub type SharedUser = Arc<RwLock<UserDict>>;

/// 组合缓冲上限(字母与隔音符总数)。
pub const MAX_RAW: usize = 40;
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
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
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
            fuzzy: Fuzzy::default(),
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
    cfg: Config,
    mode: Mode,
    comp: Option<Composition>,
    /// 当前组合里已经上屏过分段词(这时左邻文本不含这些词)。
    comp_committed: bool,
    cands: Vec<query::Candidate>,
    hl: usize,
    page: usize,
    dq_open: bool,
    sq_open: bool,
    /// 最近上屏的文本,最多 [`CONTEXT_KEEP`] 字。焦点切换时清空。
    context: String,
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
            cfg,
            mode: Mode::Chinese,
            comp: None,
            comp_committed: false,
            cands: Vec::new(),
            hl: 0,
            page: 0,
            dq_open: false,
            sq_open: false,
            context: String::new(),
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

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        if mode == Mode::English {
            self.clear_comp();
        }
    }

    pub fn composition(&self) -> Option<&Composition> {
        self.comp.as_ref()
    }

    pub fn candidates(&self) -> Vec<String> {
        self.cands.iter().map(|c| c.text.clone()).collect()
    }

    /// 带来源标记的候选。重排只应把 `full` 候选移到首位;`trusted` 是
    /// 是否由词典精确回答整个按键。
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
    }

    /// 清空已上屏上下文(焦点切到别处时调用)。
    pub fn clear_context(&mut self) {
        self.context.clear();
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

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn page_size(&self) -> usize {
        self.cfg.page_size
    }

    pub fn config(&self) -> &Config {
        &self.cfg
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

    /// 处理一次按键,返回由此产生的全部事件。
    pub fn process(&mut self, key: Key) -> Vec<Event> {
        let events = self.process_inner(key);
        for event in &events {
            if let Event::Commit(text) = event
                && !text.is_ascii()
            {
                self.push_context(text);
            }
        }
        events
    }

    fn process_inner(&mut self, key: Key) -> Vec<Event> {
        if key == Key::Shift {
            let mut events = Vec::new();
            if self.mode == Mode::Chinese
                && let Some(text) = self.commit_highlighted()
            {
                events.push(Event::Commit(text));
            }
            self.mode = if self.mode == Mode::Chinese {
                Mode::English
            } else {
                Mode::Chinese
            };
            events.push(Event::Mode(self.mode));
            return events;
        }
        if self.mode == Mode::English {
            return vec![Event::Passthrough(key)];
        }
        if self.comp.is_some() {
            self.process_composing(key)
        } else {
            self.process_idle(key)
        }
    }

    fn process_idle(&mut self, key: Key) -> Vec<Event> {
        match key {
            Key::Letter(c) if c.is_ascii_lowercase() => {
                self.comp = Some(Composition {
                    raw: c.to_string(),
                    cursor: 1,
                    display: String::new(),
                    seg_spans: Vec::new(),
                });
                self.comp_committed = false;
                self.recompute();
                Vec::new()
            }
            Key::Punct(p) => self.punct_events(p),
            _ => vec![Event::Passthrough(key)],
        }
    }

    fn process_composing(&mut self, key: Key) -> Vec<Event> {
        match key {
            Key::Letter(c) if c.is_ascii_lowercase() => {
                if let Some(comp) = self.comp.as_mut()
                    && comp.raw.len() < MAX_RAW
                {
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
                self.cands.clear();
                self.rev = self.rev.wrapping_add(1);
                vec![Event::Commit(raw)]
            }
            Key::Esc => {
                self.clear_comp();
                Vec::new()
            }
            Key::Backspace => {
                self.delete_syllable(true);
                Vec::new()
            }
            Key::Delete => {
                self.delete_syllable(false);
                Vec::new()
            }
            Key::Left => {
                self.move_cursor(-1);
                Vec::new()
            }
            Key::Right => {
                self.move_cursor(1);
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
                self.hl = self.hl.saturating_sub(1);
                Vec::new()
            }
            Key::Down => {
                if !self.cands.is_empty() {
                    self.hl = (self.hl + 1).min(self.cands.len() - 1);
                }
                Vec::new()
            }
            Key::PageUp | Key::PageDown => {
                let ps = self.cfg.page_size.max(1);
                let pages = self.cands.len().div_ceil(ps);
                if pages > 1 {
                    self.page = if key == Key::PageUp {
                        self.page.saturating_sub(1)
                    } else {
                        (self.page + 1).min(pages - 1)
                    };
                    self.hl = self.page * ps;
                }
                Vec::new()
            }
            Key::Digit(d) => self.select_digit(d),
            Key::Punct(p) => {
                let mut events = self
                    .commit_highlighted()
                    .map(|t| vec![Event::Commit(t)])
                    .unwrap_or_default();
                events.extend(self.punct_events(p));
                events
            }
            Key::Letter(_) => vec![Event::Passthrough(key)],
            // Shift 在 process 入口已处理,组合态里不会再遇到。
            Key::Shift => Vec::new(),
        }
    }

    /// 标点:开中文标点则上屏全角,否则直通。
    fn punct_events(&mut self, p: char) -> Vec<Event> {
        if !self.cfg.chinese_punct {
            return vec![Event::Passthrough(Key::Punct(p))];
        }
        match punct::map(p, &mut self.dq_open, &mut self.sq_open) {
            Some(s) => vec![Event::Commit(s.to_string())],
            None => vec![Event::Passthrough(Key::Punct(p))],
        }
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
            vec![Event::Passthrough(Key::Digit(d))]
        }
    }

    /// 上屏高亮候选;无候选时上屏原文。
    fn commit_highlighted(&mut self) -> Option<String> {
        self.comp.as_ref()?;
        if self.cands.is_empty() {
            let raw = self.comp.take().map(|c| c.raw);
            self.comp_committed = false;
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
        let learned = raw[..cut].to_string();
        self.user
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .learn(&learned, &cand.text);
        let rest = raw[cut..].trim_start_matches('\'').to_string();
        if rest.is_empty() {
            self.comp = None;
            self.comp_committed = false;
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
        } else {
            let comp = self.comp.as_mut()?;
            comp.raw = rest;
            comp.cursor = 0;
            self.comp_committed = true;
            self.recompute();
        }
        Some(cand.text)
    }

    fn try_insert_apostrophe(&mut self) {
        let ok = if let Some(comp) = self.comp.as_ref() {
            comp.seg_spans.iter().rev().any(|&(s, e)| {
                e == comp.cursor && pinyin_dict::syllables::is_syllable(&comp.raw[s..e])
            })
        } else {
            false
        };
        if ok && let Some(comp) = self.comp.as_mut() {
            comp.raw.insert(comp.cursor, '\'');
            comp.cursor += 1;
            self.recompute();
        }
    }

    /// 按音节回删/前删;光标不在音节边界时退化为单字母。
    fn delete_syllable(&mut self, left: bool) {
        let Some(comp) = self.comp.as_mut() else {
            return;
        };
        let spans = comp.seg_spans.clone();
        if left {
            if comp.cursor == 0 {
                return;
            }
            if let Some(&(s, _)) = spans.iter().rev().find(|&&(_, e)| e == comp.cursor) {
                comp.raw.drain(s..comp.cursor);
                comp.cursor = s;
            } else {
                let at = comp.cursor - 1;
                comp.raw.remove(at);
                comp.cursor = at;
            }
            while comp.cursor > 0 && comp.raw.as_bytes().get(comp.cursor - 1) == Some(&b'\'') {
                comp.raw.remove(comp.cursor - 1);
                comp.cursor -= 1;
            }
        } else {
            if comp.cursor >= comp.raw.len() {
                return;
            }
            if let Some(&(_, e)) = spans.iter().find(|&&(s, _)| s == comp.cursor) {
                comp.raw.drain(comp.cursor..e);
            } else {
                comp.raw.remove(comp.cursor);
            }
            if comp.cursor == 0 && comp.raw.starts_with('\'') {
                comp.raw.remove(0);
            }
        }
        if comp.raw.is_empty() {
            self.comp = None;
            self.comp_committed = false;
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
            self.rev = self.rev.wrapping_add(1);
        } else {
            self.recompute();
        }
    }

    /// 按音节边界移动光标。
    fn move_cursor(&mut self, dir: i32) {
        let Some(comp) = self.comp.as_mut() else {
            return;
        };
        let mut stops: Vec<usize> = comp.seg_spans.iter().flat_map(|&(s, e)| [s, e]).collect();
        stops.push(0);
        stops.push(comp.raw.len());
        stops.sort_unstable();
        stops.dedup();
        let cur = comp.cursor;
        comp.cursor = if dir < 0 {
            stops
                .iter()
                .rev()
                .find(|&&p| p < cur)
                .copied()
                .unwrap_or(cur.saturating_sub(1))
        } else {
            stops
                .iter()
                .find(|&&p| p > cur)
                .copied()
                .unwrap_or((cur + 1).min(comp.raw.len()))
        };
    }

    fn clear_comp(&mut self) {
        self.comp = None;
        self.comp_committed = false;
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
        let user = self.user.read().unwrap_or_else(|p| p.into_inner());
        let built = query::build(
            &self.dict,
            &user,
            &self.cfg.fuzzy,
            self.gram.as_deref(),
            &mut self.gram_cache,
            &comp.raw,
        );
        comp.display = built.display;
        comp.seg_spans = built
            .cands
            .first()
            .map(|c| c.spans.clone())
            .unwrap_or_else(|| segment::first_path(&comp.raw));
        self.cands = built.cands;
    }
}
