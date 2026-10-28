//! 中英/全半角切换徽标:独立小窗的轮询、摆位与渲染。
//!
//! 与候选窗共用父模块的屏幕/调色板辅助函数,但生命周期和摆放规则独立。

use super::*;

/// 当前这份徽标:显示哪个字、贴哪个光标、从何时开始计时。
#[derive(Clone)]
struct HudView {
    label: &'static str,
    caret: Option<(i32, i32, i32, i32)>,
    at: Instant,
}

pub(super) struct ModeHudPanel {
    state: Arc<State>,
    /// HWND 扩展样式只补一次。
    styled: Cell<bool>,
    /// 已处理的切换序号;同一序号不重复读取状态。
    serial: Cell<u64>,
    /// 上一帧是否在组合;组合开始/结束时挪开或贴回光标下方。
    was_composing: Cell<bool>,
    last: RefCell<Option<HudView>>,
}

impl ModeHudPanel {
    pub(super) fn new(state: Arc<State>) -> Self {
        Self {
            state,
            styled: Cell::new(false),
            serial: Cell::new(0),
            was_composing: Cell::new(false),
            last: RefCell::new(None),
        }
    }

    /// 轮询切换序号与显示期限。返回(是否重新摆位, 是否重绘)。
    pub(super) fn tick(&self) -> (bool, bool) {
        let serial = self.state.hud_serial.load(Ordering::Relaxed);
        let composing_now = in_composition(&self.state);
        let moved = self.was_composing.replace(composing_now) != composing_now;
        let mut place = false;
        if serial != self.serial.get() {
            self.serial.set(serial);
            let source = self
                .state
                .mode_hud
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(hud) = source.as_ref() {
                *self.last.borrow_mut() = Some(HudView {
                    label: hud.label,
                    caret: hud.caret,
                    at: hud.at,
                });
            }
            place = true;
        }
        let Some(view) = self.last.borrow().clone() else {
            return (place, place);
        };
        if Instant::now().saturating_duration_since(view.at) >= HUD_TOTAL {
            self.last.borrow_mut().take();
            (true, true)
        } else {
            (place || moved, true)
        }
    }

    /// 平时贴在光标下方;下方放不下就翻到上方,组合中按候选窗翻向选相反的一侧,没有光标时落在主屏下三分之一。
    pub(super) fn place(&self, window: &Window) {
        let Some(hwnd) = window_hwnd(window) else {
            return;
        };
        // 徽标永远鼠标穿透,不挡宿主的选择与点击。
        style_popup(hwnd, &self.styled, true);
        let (park_x, park_y) = park_point();
        let Some(view) = self.last.borrow().clone() else {
            park(hwnd, park_x, park_y);
            return;
        };
        let caret = view
            .caret
            .map(|(l, t, r, b)| (l.min(r), t.min(b), l.max(r), b.max(t)));
        let host_pid = (self.state.active.load(Ordering::Relaxed) >> 32) as u32;
        let (anchor_x, anchor_y, work) = match caret {
            Some((l, _, r, b)) => (l + (r - l) / 2, b, work_area_at(l, b)),
            None => {
                let work = primary_work_area();
                (
                    work.left + (work.right - work.left) / 2,
                    work.top + (work.bottom - work.top) * 2 / 3,
                    work,
                )
            }
        };
        if foreground_fullscreen_at(anchor_x, anchor_y, host_pid) {
            park(hwnd, park_x, park_y);
            return;
        }
        let scale = scale_at(anchor_x, anchor_y);
        let w = (HUD_W * scale) as i32;
        let h = (HUD_H * scale) as i32;
        let gap = (4.0 * scale) as i32;
        // 候选窗默认贴光标下方、放不下才翻到上方;徽标选相反的一侧,两者不会互相遮挡。
        let in_comp = in_composition(&self.state);
        let (layout, font, _) = ui_options(&self.state);
        let metrics = Metrics::of(font);
        let candidate_h = snapshot(&self.state)
            .map_or(0.0, |view| candidate_height(&view, layout, &metrics))
            * scale;
        let candidate_below =
            caret.is_some_and(|(_, _, _, b)| b as f32 + candidate_h <= work.bottom as f32);
        let prefer_below = !in_comp || !candidate_below;
        let x = (anchor_x - w / 2).clamp(work.left, (work.right - w).max(work.left));
        let y = (match caret {
            Some((_, t, _, b)) => {
                let below = b + gap;
                let above = t - h - gap;
                if prefer_below && below + h <= work.bottom {
                    below
                } else if !prefer_below && above >= work.top {
                    above
                } else if below + h <= work.bottom {
                    below
                } else {
                    above
                }
            }
            None => anchor_y - h / 2,
        })
        .clamp(work.top, (work.bottom - h).max(work.top));
        unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE) };
    }
}

impl Render for ModeHudPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(view) = self.last.borrow().clone() else {
            return div().into_any_element();
        };
        let appearance = theme::Appearance::of(window);
        let colors = theme::colors_with(appearance, ui_options(&self.state).2);
        let high_contrast = matches!(appearance, theme::Appearance::HighContrast);
        let opacity = if cx.reduce_motion() {
            1.0
        } else {
            let elapsed = Instant::now().saturating_duration_since(view.at);
            if elapsed >= HUD_TOTAL {
                0.0
            } else if elapsed >= HUD_SHOW {
                ((HUD_TOTAL - elapsed).as_secs_f32() / HUD_FADE.as_secs_f32()).clamp(0.0, 1.0)
            } else if elapsed < HUD_FADE_IN {
                let t = elapsed.as_secs_f32() / HUD_FADE_IN.as_secs_f32();
                HUD_START_OPACITY + (1.0 - HUD_START_OPACITY) * t
            } else {
                1.0
            }
        };
        div()
            .font_family("Microsoft YaHei UI")
            .flex()
            .items_center()
            .justify_center()
            .w(px(HUD_W))
            .h(px(HUD_H))
            .bg(rgb(colors.surface))
            .border_1()
            .border_color(rgb(colors.border))
            .rounded_lg()
            .when(!high_contrast, |el| el.shadow_md())
            .text_size(px(17.0))
            .text_color(rgb(colors.text))
            .child(view.label)
            .opacity(opacity)
            .into_any_element()
    }
}
