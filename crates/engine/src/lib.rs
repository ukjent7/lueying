//! 拼音输入引擎:键状态机 + 组合编辑 + 候选管理。平台无关,上屏以事件表达。
//!
//! 默认行为对齐微软拼音习惯:空格上屏首选、数字选词、Enter 上屏原文、
//! Backspace 按音节回删、Shift 切中英(组合中先上屏再切)、中文态标点转全角。

mod punct;
mod query;
mod segment;
mod user;

use pinyin_dict::{Dictionary, Fuzzy};

use segment::segmentations;
pub use user::UserDict;

/// 组合缓冲上限(字母与隔音符总数)。
pub const MAX_RAW: usize = 40;

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
    dict: Dictionary,
    user: UserDict,
    cfg: Config,
    mode: Mode,
    comp: Option<Composition>,
    cands: Vec<query::Cand>,
    hl: usize,
    page: usize,
    dq_open: bool,
    sq_open: bool,
}

impl Engine {
    pub fn new(dict: Dictionary, cfg: Config) -> Self {
        Self {
            dict,
            user: UserDict::new(),
            cfg,
            mode: Mode::Chinese,
            comp: None,
            cands: Vec::new(),
            hl: 0,
            page: 0,
            dq_open: false,
            sq_open: false,
        }
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
        self.user.export()
    }

    pub fn import_user(&mut self, text: &str) {
        self.user.import(text);
    }

    /// 处理一次按键,返回由此产生的全部事件。
    pub fn process(&mut self, key: Key) -> Vec<Event> {
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
                self.cands.clear();
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
            return self.comp.take().map(|c| c.raw);
        }
        self.apply_consume(self.hl.min(self.cands.len() - 1))
    }

    /// 应用第 idx 个候选:上屏文本,吃掉 consume 个音节,剩余留作组合并学习。
    fn apply_consume(&mut self, idx: usize) -> Option<String> {
        let cand = self.cands.get(idx)?.clone();
        let cut = cand.spans.get(cand.consume - 1).map(|s| s.1)?;
        let raw = self.comp.as_ref()?.raw.clone();
        let learned = raw[..cut].to_string();
        self.user.learn(&learned, &cand.text);
        let rest = raw[cut..].trim_start_matches('\'').to_string();
        if rest.is_empty() {
            self.comp = None;
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
        } else {
            let comp = self.comp.as_mut()?;
            comp.raw = rest;
            comp.cursor = 0;
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
            self.cands.clear();
            self.hl = 0;
            self.page = 0;
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
        self.cands.clear();
        self.hl = 0;
        self.page = 0;
    }

    /// 重算候选与内联显示。任何组合变化后必须调用。
    fn recompute(&mut self) {
        self.hl = 0;
        self.page = 0;
        let Some(comp) = self.comp.as_mut() else {
            self.cands.clear();
            return;
        };
        let segs = segmentations(&comp.raw);
        let built = query::build(&self.dict, &self.user, &self.cfg.fuzzy, &comp.raw, &segs);
        comp.display = built.display;
        comp.seg_spans = built
            .cands
            .first()
            .map(|c| c.spans.clone())
            .unwrap_or_else(|| {
                segs.first()
                    .map(|s| s.iter().map(|x| x.span).collect())
                    .unwrap_or_default()
            });
        self.cands = built.cands;
    }
}
