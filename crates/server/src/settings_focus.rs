//! 设置页的 Tab 次序:固定控件在前,声明式扩展字段插在「扩展」分组后。

use lueying_render::ui::Id;

use crate::settings_view::{Settings, ids};

impl Settings {
    /// Tab 走一圈的次序:头部 → 卡片 → 页脚。
    pub fn focus_order() -> Vec<Id> {
        let mut order = vec![Id::new(ids::UPDATE, 0)];
        order.extend([
            Id::new(ids::SCHEME, 0),
            Id::new(ids::AUX, 0),
            Id::new(ids::CHINESE_PUNCT, 0),
            Id::new(ids::SHIFT_TOGGLE, 0),
            Id::new(ids::TRADITIONAL, 0),
            Id::new(ids::REGION, 0),
            Id::new(ids::PAGE_SIZE, 0),
            Id::new(ids::LAYOUT, 0),
            Id::new(ids::FONT, 0),
            Id::new(ids::ACCENT, 0),
            Id::new(ids::EFFECTS, 0),
            Id::new(ids::EFFECTS_TOGGLE, 0),
            Id::new(ids::PREDICT, 0),
            Id::new(ids::RESET_FUZZY, 0),
            Id::new(ids::FUZZY, 0),
            Id::new(ids::SCRIPTS, 0),
            Id::new(ids::SCRIPTS_DIR, 0),
            Id::new(ids::CONSENT, 0),
            Id::new(ids::AUDIT, 0),
            Id::new(ids::EXT_METRICS, 0),
            Id::new(ids::OCR, 0),
            Id::new(ids::OCR_DIR, 0),
            Id::new(ids::PHRASES, 0),
            Id::new(ids::PUNCT, 0),
            Id::new(ids::APPS, 0),
            Id::new(ids::BACKUP_EXPORT, 0),
            Id::new(ids::BACKUP_IMPORT, 0),
            Id::new(ids::IMPORT_USER, 0),
            Id::new(ids::EXPORT_USER, 0),
            Id::new(ids::CLEAR_USER, 0),
            Id::new(ids::DIAGNOSTICS, 0),
            Id::new(ids::CHANNEL, 0),
        ]);
        order.push(Id::new(ids::CLOSE, 0));
        order.push(Id::new(ids::SAVE, 0));
        order
    }

    /// 实际页面上的焦点次序:核心控件 + 当前扩展字段。
    pub fn focus_ids(&self) -> Vec<Id> {
        let mut order = Self::focus_order();
        let ext = self
            .ext_fields()
            .into_iter()
            .map(|field| Id::new(ids::ext_field(field.slot), 0));
        let at = order
            .iter()
            .position(|id| id.base == ids::PHRASES)
            .unwrap_or(order.len().saturating_sub(2));
        order.splice(at..at, ext);
        order
    }
}
