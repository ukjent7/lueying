//! 设置窗的纯控件:设置行、分段组、开关与按钮。
//! 只做布局与配色,不持有面板状态;焦点标记与点击动作由调用方传入。

use gpui::{App, ClickEvent, FontWeight, SharedString, Window, div, prelude::*, px, rgb};
use lueying_server::theme;

/// 设置按钮的四种外观。
#[derive(Clone, Copy)]
pub(super) enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

pub(super) fn section_label(
    title: &'static str,
    colors: theme::Colors,
    first: bool,
) -> impl IntoElement {
    div()
        .px(px(14.0))
        .pt(px(7.0))
        .pb(px(2.0))
        .when(!first, |el| {
            el.border_t_1().border_color(rgb(colors.border))
        })
        .text_size(px(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(colors.text_tertiary))
        .child(title)
}

pub(super) fn setting_row(
    label: &'static str,
    detail: impl Into<SharedString>,
    control: impl IntoElement,
    colors: theme::Colors,
    last: bool,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(39.0))
        .px(px(14.0))
        .when(!last, |el| el.border_b_1().border_color(rgb(colors.border)))
        .child(
            div()
                .flex()
                .flex_1()
                .flex_col()
                .gap(px(1.0))
                .min_w(px(0.0))
                .child(
                    div()
                        .text_size(px(13.5))
                        .text_color(rgb(colors.text))
                        .child(label),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(230.0))
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(rgb(colors.text_secondary))
                        .child(detail.into()),
                ),
        )
        .child(control)
}

pub(super) fn toggle(
    id: &'static str,
    on: bool,
    focused: bool,
    colors: theme::Colors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .w(px(38.0))
        .h(px(22.0))
        .p(px(3.0))
        .rounded_full()
        .cursor_pointer()
        .bg(rgb(if on { colors.accent } else { colors.off_bg }))
        .when(focused, |el| el.border_2().border_color(rgb(colors.text)))
        .when(on, |el| el.justify_end())
        .when(!on, |el| el.justify_start())
        .hover(|style| style.opacity(0.9))
        .active(|style| style.opacity(0.75))
        .on_click(on_click)
        .child(
            div()
                .w(px(16.0))
                .h(px(16.0))
                .rounded_full()
                .bg(rgb(if on {
                    colors.on_accent
                } else {
                    colors.knob_off
                }))
                .shadow_xs(),
        )
}

pub(super) fn segment(
    id_base: &'static str,
    value: usize,
    label: impl Into<SharedString>,
    width: f32,
    selected: bool,
    high_contrast: bool,
    colors: theme::Colors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label: SharedString = label.into();
    let (selected_bg, selected_text) = if high_contrast {
        (colors.accent, colors.on_accent)
    } else {
        (colors.segment, colors.text)
    };
    div()
        .id((id_base, value))
        .flex()
        .items_center()
        .justify_center()
        .w(px(width))
        .h(px(26.0))
        .rounded_md()
        .text_size(px(12.0))
        .cursor_pointer()
        .when(selected, |el| {
            el.bg(rgb(selected_bg))
                .text_color(rgb(selected_text))
                .when(!high_contrast, |el| el.shadow_xs())
        })
        .when(!selected, |el| {
            el.text_color(rgb(colors.text_secondary))
                .hover(|style| style.text_color(rgb(colors.text)))
        })
        .active(|style| style.opacity(0.8))
        .on_click(on_click)
        .child(label)
}

pub(super) fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
    focused: bool,
    colors: theme::Colors,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label: SharedString = label.into();
    let (bg, text) = match kind {
        ButtonKind::Primary => (colors.accent, colors.on_accent),
        ButtonKind::Secondary => (colors.surface, colors.text),
        ButtonKind::Ghost => (colors.surface, colors.text_secondary),
        ButtonKind::Danger => (colors.danger_soft, colors.danger),
    };
    let hover = match kind {
        ButtonKind::Primary => colors.accent_hover,
        ButtonKind::Secondary => colors.hover,
        // Ghost 没有底色,悬停要用比窗口底更深一档的中性色才看得见。
        ButtonKind::Ghost => colors.border,
        ButtonKind::Danger => colors.danger_soft,
    };
    // 焦点环:主色按钮底色就是 accent,环用反色才看得见;其余用 accent。
    let ring = if focused {
        if matches!(kind, ButtonKind::Primary) {
            colors.on_accent
        } else {
            colors.accent
        }
    } else {
        colors.border
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(30.0))
        .px(px(12.0))
        .rounded_md()
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .when(!matches!(kind, ButtonKind::Ghost), |el| el.bg(rgb(bg)))
        .text_color(rgb(if enabled { text } else { colors.text_tertiary }))
        .when(focused, |el| el.border_2().border_color(rgb(ring)))
        .when(matches!(kind, ButtonKind::Secondary) && !focused, |el| {
            el.border_1().border_color(rgb(colors.border_strong))
        })
        .when(enabled, |el| {
            el.cursor_pointer()
                .when(matches!(kind, ButtonKind::Danger), |el| {
                    el.hover(|style| style.opacity(0.9))
                })
                .when(!matches!(kind, ButtonKind::Danger), |el| {
                    el.hover(move |style| style.bg(rgb(hover)))
                })
                .active(|style| style.opacity(0.8))
                .on_click(on_click)
        })
        .child(label)
}

/// 组名转为设置页可读的模糊对。
pub(super) fn group_label(name: &str) -> &'static str {
    match name {
        "zcs" => "z/zh c/ch s/sh",
        "ln" => "l/n",
        "fh" => "f/h",
        "rl" => "r/l",
        "an_ang" => "an/ang",
        "en_eng" => "en/eng",
        "in_ing" => "in/ing",
        _ => "其他",
    }
}
