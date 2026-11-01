//! 命令面板的搜索模型:输入拼音,用引擎候选匹配命令标题。
//!
//! 面板自带输入框,不经过 TSF 会话:字母逐个喂给一台关闭学习的引擎,
//! 引擎给出的候选按顺序去匹配命令标题;标题里的拉丁字母直接按子串匹配。
//! 模型不碰窗口与平台 API,Linux 单测与 Windows 窗口共用同一份逻辑。

use std::sync::Arc;

use lueying_dict::Dictionary;
use lueying_engine::{Config, Engine, Key};

/// 单次查询的字母上限;命令标题的拼音远短于这个数,超出的键直接忽略。
const MAX_QUERY: usize = 32;

/// 面板里的一条命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandItem {
    /// 完整命令 id `<扩展 id>.<命令 id>`。
    pub id: String,
    /// 命令标题,搜索与显示都用它。
    pub title: String,
    /// 来源扩展 id,显示在标题右侧。
    pub extension: String,
}

/// 一屏可见的一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRow {
    pub title: String,
    pub extension: String,
}

/// 面板的一帧视图:查询串、可见行与可见区里的选中下标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandView {
    /// 输入框里的原文。
    pub query: String,
    /// 当前可见的行。
    pub rows: Vec<CommandRow>,
    /// 选中行在 `rows` 里的下标;没有匹配时为 0。
    pub selected: usize,
    /// 匹配总数。
    pub total: usize,
    /// 可见区上方被卷掉的行数。
    pub above: usize,
    /// 可见区下方还有多少行。
    pub below: usize,
}

/// 命令面板的搜索状态:一台独立引擎、命令表、查询串与匹配结果。
pub struct CommandPanel {
    dict: Arc<Dictionary>,
    engine: Engine,
    items: Vec<CommandItem>,
    query: String,
    /// 匹配到的命令下标,按相关度排序。
    matches: Vec<usize>,
    selected: usize,
    /// 可见区起点;由 [`CommandPanel::view`] 按窗口行数收紧。
    offset: usize,
}

impl CommandPanel {
    /// 建面板;命令表为空时面板仍可用,只是没有行可显示。
    pub fn new(dict: Arc<Dictionary>, items: Vec<CommandItem>) -> Self {
        let engine = Self::fresh_engine(&dict);
        let mut panel = Self {
            dict,
            engine,
            items,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            offset: 0,
        };
        panel.refresh();
        panel
    }

    /// 换一份命令表;查询与选中位置保留,匹配结果重算。
    pub fn set_items(&mut self, items: Vec<CommandItem>) {
        self.items = items;
        self.refresh();
    }

    /// 清空查询,回到「列出全部命令」的状态。
    pub fn reset(&mut self) {
        self.query.clear();
        self.engine = Self::fresh_engine(&self.dict);
        self.selected = 0;
        self.offset = 0;
        self.refresh();
    }

    /// 当前查询串。
    pub fn query(&self) -> &str {
        &self.query
    }

    /// 匹配总数。
    pub fn matches(&self) -> usize {
        self.matches.len()
    }

    /// 输入一个字符;字母同时喂给引擎,其他可见字符只参与直接匹配。
    /// 返回查询是否变化(超出上限或不可见字符返回 `false`)。
    pub fn type_char(&mut self, ch: char) -> bool {
        if ch.is_control() {
            return false;
        }
        if self.query.chars().count() >= MAX_QUERY {
            return false;
        }
        self.query.push(ch);
        if ch.is_ascii_alphabetic() {
            self.engine.process(Key::Letter(ch.to_ascii_lowercase()));
        }
        self.selected = 0;
        self.offset = 0;
        self.refresh();
        true
    }

    /// 删掉查询的最后一个字符;空查询返回 `false`。
    pub fn backspace(&mut self) -> bool {
        let Some(ch) = self.query.pop() else {
            return false;
        };
        if ch.is_ascii_alphabetic() {
            self.engine.process(Key::Backspace);
        }
        self.selected = 0;
        self.offset = 0;
        self.refresh();
        true
    }

    /// 上下移动选中行;到头就停住,返回是否真的移动。
    pub fn move_by(&mut self, delta: isize) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        let last = self.matches.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, last);
        let changed = next != self.selected as isize;
        self.selected = next as usize;
        changed
    }

    /// 选中行的命令。
    pub fn selected_item(&self) -> Option<&CommandItem> {
        self.matches
            .get(self.selected)
            .and_then(|index| self.items.get(*index))
    }

    /// 取一帧视图;`max_rows` 是窗口能放下的行数,卷动位置据此收紧。
    pub fn view(&mut self, max_rows: usize) -> CommandView {
        let max_rows = max_rows.max(1);
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + max_rows {
            self.offset = self.selected + 1 - max_rows;
        }
        self.offset = self.offset.min(self.matches.len().saturating_sub(max_rows));
        let end = (self.offset + max_rows).min(self.matches.len());
        let rows = self.matches[self.offset..end]
            .iter()
            .filter_map(|index| self.items.get(*index))
            .map(|item| CommandRow {
                title: item.title.clone(),
                extension: item.extension.clone(),
            })
            .collect();
        CommandView {
            query: self.query.clone(),
            rows,
            selected: self.selected.saturating_sub(self.offset),
            total: self.matches.len(),
            above: self.offset,
            below: self.matches.len().saturating_sub(end),
        }
    }

    /// 一台关闭学习的新引擎;面板的查询不进用户词库。
    fn fresh_engine(dict: &Arc<Dictionary>) -> Engine {
        let mut engine = Engine::new(Arc::clone(dict), Config::default());
        engine.set_learning(false);
        engine
    }

    /// 重算匹配:纯字母查询用引擎候选,所有查询都做直接子串匹配。
    fn refresh(&mut self) {
        let query = self.query.to_lowercase();
        if query.is_empty() {
            self.matches = (0..self.items.len()).collect();
            self.selected = 0;
            self.offset = 0;
            return;
        }
        let candidates: Vec<String> = if self.query.chars().all(|ch| ch.is_ascii_alphabetic()) {
            self.engine
                .candidates()
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect()
        } else {
            // 混入数字/符号时只做直接匹配:引擎候选仍停在最后一段拼音上,
            // 继续用它们会让列表对不上输入框里的内容。
            Vec::new()
        };
        // 排序键:(直接匹配 0 / 候选匹配 1, -候选长度, 候选序号, 标题内位置, 命令下标)。
        let mut hits: Vec<(u32, i32, usize, usize, usize)> = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            let title = item.title.to_lowercase();
            let mut best: Option<(u32, i32, usize, usize)> = None;
            if let Some(pos) = title.find(&query) {
                best = Some((0, 0, 0, pos));
            }
            for (rank, candidate) in candidates.iter().enumerate() {
                let candidate = candidate.to_lowercase();
                let Some(pos) = title.find(&candidate) else {
                    continue;
                };
                // 更长的候选更具体;同长度再按引擎给出的顺序。
                let key = (1, -(candidate.chars().count() as i32), rank, pos);
                if best.is_none_or(|current| key < current) {
                    best = Some(key);
                }
            }
            if let Some((class, length, rank, pos)) = best {
                hits.push((class, length, rank, pos, index));
            }
        }
        hits.sort();
        self.matches = hits.into_iter().map(|(_, _, _, _, index)| index).collect();
        self.selected = 0;
        self.offset = 0;
    }
}

#[cfg(test)]
#[path = "command_panel_tests.rs"]
mod tests;
