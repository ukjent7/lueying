//! 系统字型:候选窗、模式徽标与设置窗共用一套取字型的办法。
//!
//! 微软雅黑是系统自带的字型,覆盖全拼、双拼与界面文案;系统里没有这一族
//! 就退回默认字型——拿不到字型就没有字,窗口会空成一块底色,比字体难看得多。

use skia_safe::{Font, FontMgr, FontStyle};

/// 取面板用的字型;系统里一个可用字型都没有时返回 None。
pub fn panel_font() -> Option<Font> {
    let mgr = FontMgr::new();
    let face = mgr
        .match_family_style("Microsoft YaHei UI", FontStyle::normal())
        .or_else(|| mgr.legacy_make_typeface(None, FontStyle::normal()))?;
    Some(Font::from_typeface(face, 17.0))
}

/// 兜底字型:没有微软雅黑也要有字。
pub fn ui_font() -> Font {
    panel_font().unwrap_or_default()
}
