//! 宿主选择面板的页面模型:标题、条目、选中与滚动。
//!
//! 平台无关;Windows 窗口只负责按键、鼠标和绘制。扩展通过
//! `panel.show({title, items})` 请求,用户挑选后回传下标,取消回传 `null`。

/// 标题最多多少个字符。
pub const MAX_TITLE: usize = 120;
/// 最多多少个条目。
pub const MAX_ITEMS: usize = 64;
/// 单条最多多少个字符。
pub const MAX_ITEM: usize = 200;

/// 一次选择面板的输入;由扩展参数解析而来。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPanel {
    title: String,
    items: Vec<String>,
    selected: usize,
    offset: usize,
}

/// 一帧可见内容;`selected` 是 `rows` 里的下标。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPanelView {
    pub title: String,
    pub rows: Vec<String>,
    pub selected: usize,
    pub total: usize,
    pub above: usize,
    pub below: usize,
}

impl HostPanel {
    /// 解析 `{"title":"...","items":["..."]}`;标题和条目都裁剪首尾空白。
    pub fn parse(args: &str) -> Result<Self, String> {
        let parsed: serde_json::Value =
            serde_json::from_str(args).map_err(|error| format!("参数不是 JSON: {error}"))?;
        let title = parsed
            .get("title")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "缺 title".to_string())?
            .trim();
        if title.is_empty() {
            return Err("title 不能为空".to_string());
        }
        if title.chars().count() > MAX_TITLE {
            return Err(format!("title 超过 {MAX_TITLE} 个字符"));
        }
        let items = parsed
            .get("items")
            .and_then(|value| value.as_array())
            .ok_or_else(|| "缺 items".to_string())?;
        if items.is_empty() {
            return Err("items 不能为空".to_string());
        }
        if items.len() > MAX_ITEMS {
            return Err(format!("items 超过 {MAX_ITEMS} 项"));
        }
        let mut texts = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let text = item
                .as_str()
                .ok_or_else(|| format!("items[{index}] 不是字符串"))?
                .trim();
            if text.is_empty() {
                return Err(format!("items[{index}] 不能为空"));
            }
            if text.chars().count() > MAX_ITEM {
                return Err(format!("items[{index}] 超过 {MAX_ITEM} 个字符"));
            }
            texts.push(text.to_string());
        }
        Ok(Self {
            title: title.to_string(),
            items: texts,
            selected: 0,
            offset: 0,
        })
    }

    /// 当前标题。
    pub fn title(&self) -> &str {
        &self.title
    }

    /// 当前选中的条目下标。
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// 上下移动选中行;到头就停住,返回是否真的移动。
    pub fn move_by(&mut self, delta: isize) -> bool {
        let last = self.items.len().saturating_sub(1) as isize;
        let next = (self.selected as isize + delta).clamp(0, last);
        let changed = next != self.selected as isize;
        self.selected = next as usize;
        changed
    }

    /// 取一帧视图;`max_rows` 是窗口能放下的行数,卷动位置据此收紧。
    pub fn view(&mut self, max_rows: usize) -> HostPanelView {
        let max_rows = max_rows.max(1);
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + max_rows {
            self.offset = self.selected + 1 - max_rows;
        }
        self.offset = self.offset.min(self.items.len().saturating_sub(max_rows));
        let end = (self.offset + max_rows).min(self.items.len());
        HostPanelView {
            title: self.title.clone(),
            rows: self.items[self.offset..end].to_vec(),
            selected: self.selected.saturating_sub(self.offset),
            total: self.items.len(),
            above: self.offset,
            below: self.items.len().saturating_sub(end),
        }
    }
}

#[cfg(test)]
#[path = "host_panel_tests.rs"]
mod tests;
