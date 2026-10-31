//! 设置页的 Tab 次序:由当前页面模型自动生成。
//!
//! 左栏、当前页控件与页脚都从 `Page::focus_ids` 来,新增设置行不用再
//! 手工维护第二张焦点表。

use lueying_render::ui::Id;

use crate::settings_view::{PageColors, Settings};

impl Settings {
    /// 当前页面的焦点次序。
    pub fn focus_ids(&self) -> Vec<Id> {
        self.page(&PageColors::default()).focus_ids()
    }
}
