//! 候选窗的鼠标交互:悬停、点选、展开/滚动与右键菜单。
//!
//! 命中几何由渲染核心给(`lueying_render::panel`),这里只把命中翻译成
//! 服务端请求;窗口、摆位与绘制都在 `ui::d2d`。

use std::sync::atomic::Ordering;

use lueying_protocol::Request;
use lueying_render::panel;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, SetCursor, WM_CAPTURECHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_SETCURSOR,
};

use crate::imp::State;
use crate::ui::d2d::{Geom, WinCtx, menu_origin, redraw};
use crate::ui::place;

/// 可交互:有激活会话且服务端候选窗在显示(组合中)。
fn interactive(state: &State) -> bool {
    state.active.load(Ordering::Relaxed) != 0 && place::composing(state)
}

/// 鼠标位置是否落在可点区域(候选行/箭头/菜单);WM_SETCURSOR 用。
fn hand_hit(ctx: &WinCtx, x: f32, y: f32) -> bool {
    let Some(geom) = ctx.fx.geometry.as_ref() else {
        return false;
    };
    // 滚动条用箭头光标;它的隐形命中区压过最右一列候选的右缘。
    if scrollbar_hovered(ctx, geom, x, y) {
        return false;
    }
    if !matches!(
        panel::hit_test(&geom.spec, &geom.metrics, &ctx.text, x, y),
        panel::Hit::Nothing
    ) {
        return true;
    }
    geom.menu_xy
        .is_some_and(|(mx, my)| panel::menu_hit(mx, my, &geom.metrics, x, y).is_some())
}

/// 当前帧的滚动条几何(画布坐标);与绘制共用同一份布局。
fn scrollbar_geometry(ctx: &WinCtx, geom: &Geom) -> Option<(panel::Scroll, f32, f32)> {
    let scroll = geom.spec.scroll?;
    let width = panel::panel_width(&geom.spec, &geom.metrics, &ctx.text);
    let height = panel::panel_height(&geom.spec, &geom.metrics, &ctx.text);
    Some((scroll, width, height))
}

/// 鼠标是否落在滚动条的隐形命中区里;收起钮优先,右缘重叠处不算滚动条。
fn scrollbar_hovered(ctx: &WinCtx, geom: &Geom, x: f32, y: f32) -> bool {
    if matches!(
        panel::hit_test(&geom.spec, &geom.metrics, &ctx.text, x, y),
        panel::Hit::Expand
    ) {
        return false;
    }
    scrollbar_geometry(ctx, geom).is_some_and(|(scroll, width, height)| {
        panel::scrollbar_hit(scroll, width, height, &geom.metrics, x, y).is_some()
    })
}

/// 左键命中后的动作;菜单项优先于候选行。
enum Click {
    Menu {
        item: usize,
        slot: usize,
    },
    Pick(usize),
    /// 展开网格里的候选;带全量候选里的绝对序号。
    PickIndex(usize),
    /// 条尾 ⌄:切固定条/展开网格。
    Expand,
    Nothing,
}

/// 鼠标消息:悬停反馈、点选候选、展开/滚动与右键菜单。
pub(super) fn on_mouse(
    ctx: &mut WinCtx,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let x = (lparam.0 & 0xffff) as u16 as i16 as i32 as f32;
    let y = ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32 as f32;
    match msg {
        WM_MOUSEMOVE => {
            ctx.fx.mouse = (x, y);
            if ctx.fx.fading() {
                return LRESULT(0);
            }
            // 拖动滚动条:鼠标可能移出窗口,坐标按有符号解析。
            if let Some(grab) = ctx.fx.scroll_drag {
                if let Some(geom) = ctx.fx.geometry.as_ref()
                    && let Some((scroll, width, height)) = scrollbar_geometry(ctx, geom)
                {
                    let offset = panel::scrollbar_offset(
                        scroll,
                        width,
                        height,
                        &geom.metrics,
                        y,
                        Some(grab),
                    );
                    if offset != scroll.offset {
                        let session = ctx.state.active.load(Ordering::Relaxed);
                        let _ =
                            crate::imp::handle(&ctx.state, Request::ScrollTo { session, offset });
                    }
                }
                return LRESULT(0);
            }
            let scroll_hover = match ctx.fx.geometry.as_ref() {
                Some(geom) => scrollbar_hovered(ctx, geom, x, y),
                None => false,
            };
            let (item, chev, menu_hover) = match ctx.fx.geometry.as_ref() {
                Some(geom) => {
                    let menu_hover = geom
                        .menu_xy
                        .and_then(|(mx, my)| panel::menu_hit(mx, my, &geom.metrics, x, y));
                    // 菜单项与滚动条压住的行不参与悬停,避免底下还有高亮跟着动。
                    let hit = if menu_hover.is_some() || scroll_hover {
                        panel::Hit::Nothing
                    } else {
                        panel::hit_test(&geom.spec, &geom.metrics, &ctx.text, x, y)
                    };
                    let item = match hit {
                        panel::Hit::Item(index) => Some(index),
                        _ => None,
                    };
                    let chev = matches!(hit, panel::Hit::Expand);
                    (item, chev, menu_hover)
                }
                None => (None, false, None),
            };
            if item != ctx.fx.hover
                || chev != ctx.fx.chev
                || menu_hover != ctx.fx.menu_hover
                || scroll_hover != ctx.fx.scroll_hover
            {
                ctx.fx.hover = item;
                ctx.fx.chev = chev;
                ctx.fx.menu_hover = menu_hover;
                ctx.fx.scroll_hover = scroll_hover;
                redraw(ctx);
            }
            // 鼠标离开窗口时清掉悬停态,滚动条不会停在加宽态。
            unsafe {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if ctx.fx.scroll_hover
                || ctx.fx.hover.is_some()
                || ctx.fx.chev
                || ctx.fx.menu_hover.is_some()
            {
                ctx.fx.scroll_hover = false;
                ctx.fx.hover = None;
                ctx.fx.chev = false;
                ctx.fx.menu_hover = None;
                redraw(ctx);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            // 命中候选/箭头/菜单给手型,其余交回系统;坐标用最近的 WM_MOUSEMOVE。
            let (mx, my) = ctx.fx.mouse;
            let hand = hand_hit(ctx, mx, my);
            let cursor = if hand {
                ctx.cursor_hand
            } else {
                ctx.cursor_arrow
            };
            if let Some(cursor) = cursor {
                unsafe { SetCursor(Some(cursor)) };
            }
            LRESULT(if hand { 1 } else { 0 })
        }
        WM_LBUTTONDOWN => {
            if !interactive(&ctx.state) {
                return LRESULT(0);
            }
            let session = ctx.state.active.load(Ordering::Relaxed);
            // 滚动条优先:滑块直接拖,轨道空白处跳转后继续拖。
            let drag = match ctx.fx.geometry.as_ref() {
                Some(geom) => {
                    let over_chevron = matches!(
                        panel::hit_test(&geom.spec, &geom.metrics, &ctx.text, x, y),
                        panel::Hit::Expand
                    );
                    if over_chevron {
                        None
                    } else {
                        scrollbar_geometry(ctx, geom).and_then(|(scroll, width, height)| {
                            let hit =
                                panel::scrollbar_hit(scroll, width, height, &geom.metrics, x, y)?;
                            let (_, thumb) =
                                panel::scrollbar_rects(scroll, width, height, &geom.metrics, true)?;
                            let grab = match hit {
                                panel::ScrollHit::Thumb => {
                                    (y - thumb.top).clamp(0.0, thumb.height())
                                }
                                panel::ScrollHit::Track => {
                                    let grab = thumb.height() * 0.5;
                                    let offset = panel::scrollbar_offset(
                                        scroll,
                                        width,
                                        height,
                                        &geom.metrics,
                                        y,
                                        Some(grab),
                                    );
                                    let _ = crate::imp::handle(
                                        &ctx.state,
                                        Request::ScrollTo { session, offset },
                                    );
                                    grab
                                }
                            };
                            Some(grab)
                        })
                    }
                }
                None => None,
            };
            if let Some(grab) = drag {
                ctx.fx.scroll_drag = Some(grab);
                ctx.fx.scroll_hover = true;
                unsafe {
                    SetCapture(hwnd);
                }
                redraw(ctx);
                return LRESULT(0);
            }
            let click = match ctx.fx.geometry.as_ref() {
                Some(geom) => {
                    if let Some((mx, my)) = geom.menu_xy
                        && let Some(item) = panel::menu_hit(mx, my, &geom.metrics, x, y)
                    {
                        Click::Menu {
                            item,
                            slot: ctx.fx.menu.as_ref().map_or(0, |(slot, _, _)| *slot),
                        }
                    } else {
                        match panel::hit_test(&geom.spec, &geom.metrics, &ctx.text, x, y) {
                            panel::Hit::Item(slot) => Click::Pick(slot),
                            panel::Hit::GridItem(index) => Click::PickIndex(index),
                            panel::Hit::Expand => Click::Expand,
                            panel::Hit::Nothing => Click::Nothing,
                        }
                    }
                }
                None => Click::Nothing,
            };
            match click {
                Click::Menu { item, slot } => {
                    let request = if item == 0 {
                        Request::Pin { session, slot }
                    } else {
                        Request::Demote { session, slot }
                    };
                    let _ = crate::imp::handle(&ctx.state, request);
                    ctx.fx.menu = None;
                    ctx.fx.menu_hover = None;
                    redraw(ctx);
                }
                Click::Pick(slot) => {
                    let _ = crate::imp::handle(&ctx.state, Request::Pick { session, slot });
                }
                Click::PickIndex(index) => {
                    let _ = crate::imp::handle(&ctx.state, Request::PickIndex { session, index });
                }
                Click::Expand => {
                    let _ = crate::imp::handle(&ctx.state, Request::Expand { session });
                }
                Click::Nothing => {}
            }
            // 回落宿主没有 TSF 的 Sync 来取点选结果,点完当场注入。
            crate::imp::fallback::flush(&ctx.state);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if ctx.fx.scroll_drag.take().is_some() {
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // 别的窗口抢走捕获(切换窗口等)时结束拖动,避免状态卡住。
            ctx.fx.scroll_drag = None;
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            if !interactive(&ctx.state) {
                return LRESULT(0);
            }
            // 固定条塞不下菜单:先把窗口切到展开态,菜单一帧后锚在整词行上
            // (draw_view 按「槽位 + 行文本」重新取锚,内容对不上自动失效)。
            let session = ctx.state.active.load(Ordering::Relaxed);
            let target = match ctx.fx.geometry.as_ref() {
                Some(geom) => match panel::hit_test(&geom.spec, &geom.metrics, &ctx.text, x, y) {
                    panel::Hit::Item(slot) => geom.spec.items.get(slot).map(|item| {
                        let strip =
                            matches!(geom.spec.layout, panel::Layout::Wrap) && !geom.spec.expand;
                        if strip {
                            let _ = crate::imp::handle(&ctx.state, Request::Expand { session });
                        }
                        let (_, top) = menu_origin(slot, &geom.spec, &geom.metrics, &ctx.text);
                        (slot, top, item.text.clone())
                    }),
                    _ => None,
                },
                None => None,
            };
            if let Some((slot, top, text)) = target {
                ctx.fx.menu = Some((slot, top, text));
                ctx.fx.menu_hover = None;
                redraw(ctx);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // 高 16 位是有符号滚轮增量;固定条上滚轮展开棋盘,
            // 棋盘里滚轮翻整页。
            let delta = (wparam.0 >> 16) as u16 as i16;
            if delta != 0 && interactive(&ctx.state) {
                let session = ctx.state.active.load(Ordering::Relaxed);
                let Some(published) = ctx.state.active_published() else {
                    return LRESULT(0);
                };
                let (expanded, len) = (published.view.expanded, published.view.candidates.len());
                if len > 1 {
                    if expanded {
                        let _ = crate::imp::handle(
                            &ctx.state,
                            Request::Page {
                                session,
                                delta: if delta > 0 { -1 } else { 1 },
                            },
                        );
                    } else {
                        let _ = crate::imp::handle(&ctx.state, Request::Expand { session });
                    }
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
