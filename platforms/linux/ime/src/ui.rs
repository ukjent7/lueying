//! 候选窗的平台无关部分:把会话快照切成页内条目并估宽。
//!
//! 绘制与命中仍由 `lueying-render::panel` 完成;这里只决定这一帧显示什么。
//! 单独成模块是为了能在无 Wayland 的单元测试里覆盖翻页、页标签与宽度稳定。

use lueying_protocol::SessionView;
use lueying_render::panel::{self, Item, Layout, Measure, Metrics, PanelSpec, PreeditBar};

/// 候选文字的最大绘制宽度(逻辑像素)。
pub const MAX_TEXT_WIDTH: f32 = 490.0;

/// 当前页的绘制规格;空候选时返回空条目列表。
pub fn spec_of(view: &SessionView, layout: Layout) -> PanelSpec {
    let page_size = view.page_size.max(1);
    let start = (view.page * page_size).min(view.candidates.len());
    let end = (start + page_size).min(view.candidates.len());
    let items = (start..end)
        .map(|index| Item {
            text: view.candidates[index].clone(),
            hint: view.hints.get(index).cloned().unwrap_or_default(),
            selected: index == view.highlight,
        })
        .collect();
    let pages = view.candidates.len().div_ceil(page_size).max(1);
    PanelSpec {
        width: 0.0,
        max_text_width: MAX_TEXT_WIDTH,
        preedit: PreeditBar {
            text: view.raw.clone(),
            page_label: if pages > 1 {
                format!("{}/{}", view.page + 1, pages)
            } else {
                String::new()
            },
        },
        items,
        layout,
    }
}

/// 全部候选参与估宽,翻页时窗口宽度保持稳定。
pub fn measure_width(view: &SessionView, metrics: &Metrics, measure: &dyn Measure) -> f32 {
    let mut spec = spec_of(view, Layout::Wrap);
    spec.items = view
        .candidates
        .iter()
        .enumerate()
        .map(|(index, text)| Item {
            text: text.clone(),
            hint: view.hints.get(index).cloned().unwrap_or_default(),
            selected: false,
        })
        .collect();
    panel::panel_width(&spec, metrics, measure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lueying_render::panel::Estimate;

    fn view(page: usize, highlight: usize) -> SessionView {
        SessionView {
            raw: "nihao".into(),
            candidates: vec![
                "你".into(),
                "你好".into(),
                "拟".into(),
                "泥".into(),
                "逆".into(),
            ],
            hints: vec![
                "ni".into(),
                "ni hao".into(),
                String::new(),
                String::new(),
                String::new(),
            ],
            page,
            page_size: 2,
            highlight,
            ..SessionView::default()
        }
    }

    #[test]
    fn page_slice_and_selection_follow_the_view() {
        let spec = spec_of(&view(1, 2), Layout::Wrap);
        assert_eq!(spec.items.len(), 2);
        assert_eq!(spec.items[0].text, "拟");
        assert_eq!(spec.items[1].text, "泥");
        assert!(spec.items[0].selected);
        assert!(!spec.items[1].selected);
        assert_eq!(spec.preedit.text, "nihao");
        assert_eq!(spec.preedit.page_label, "2/3");
    }

    #[test]
    fn last_page_is_clipped_to_available_candidates() {
        let spec = spec_of(&view(2, 4), Layout::Wrap);
        assert_eq!(spec.items.len(), 1);
        assert_eq!(spec.items[0].text, "逆");
        assert!(spec.items[0].selected);
    }

    #[test]
    fn single_page_has_no_page_label() {
        let mut view = view(0, 0);
        view.candidates.truncate(2);
        let spec = spec_of(&view, Layout::Wrap);
        assert!(spec.preedit.page_label.is_empty());
    }

    #[test]
    fn width_is_stable_across_pages() {
        let metrics = Metrics::standard();
        let first = measure_width(&view(0, 0), &metrics, &Estimate);
        let second = measure_width(&view(2, 0), &metrics, &Estimate);
        assert_eq!(first, second);
    }

    #[test]
    fn column_layout_keeps_the_same_items() {
        let wrap = spec_of(&view(0, 1), Layout::Wrap);
        let column = spec_of(&view(0, 1), Layout::Column);
        assert_eq!(wrap.items.len(), column.items.len());
        assert_eq!(wrap.items[1].selected, column.items[1].selected);
    }
}
