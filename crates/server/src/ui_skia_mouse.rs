//! 候选窗的鼠标交互:悬停、点选、翻页与右键菜单。
//!
//! 命中几何由渲染核心给(`lueying_render::panel`),这里只把命中翻译成
//! 服务端请求;窗口、摆位与绘制都在 `ui_skia`。

use std::sync::atomic::Ordering;

use lueying_protocol::Request;
use lueying_render::panel;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, SetCursor, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN,
    WM_SETCURSOR,
};

use crate::imp::State;
use crate::ui_place;
use crate::ui_skia::{WinCtx, menu_origin};

/// 可交互:有激活会话且服务端候选窗在显示(组合中)。
fn interactive(state: &State) -> bool {
    state.active.load(Ordering::Relaxed) != 0 && ui_place::composing(state)
}

/// 鼠标位置是否落在可点区域(候选行/箭头/菜单);WM_SETCURSOR 用。
fn hand_hit(ctx: &WinCtx, x: f32, y: f32) -> bool {
    let Some(geom) = ctx.fx.geometry.as_ref() else {
        return false;
    };
    if !matches!(
        panel::hit_test(&geom.spec, &geom.metrics, ctx.font.as_ref(), x, y),
        panel::Hit::Nothing
    ) {
        return true;
    }
    geom.menu_xy
        .is_some_and(|(mx, my)| panel::menu_hit(mx, my, &geom.metrics, x, y).is_some())
}

/// 左键命中后的动作;菜单项优先于候选行。
enum Click {
    Menu { item: usize, slot: usize },
    Pick(usize),
    Page(i32),
    Nothing,
}

/// 鼠标消息:悬停反馈、点选候选、翻页与右键菜单。
pub(super) fn on_mouse(
    ctx: &mut WinCtx,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let x = (lparam.0 & 0xffff) as u16 as i32 as f32;
    let y = ((lparam.0 >> 16) & 0xffff) as u16 as i32 as f32;
    match msg {
        WM_MOUSEMOVE => {
            ctx.fx.mouse = (x, y);
            if ctx.fx.hide.is_some() {
                return LRESULT(0);
            }
            let (item, pager, menu_hover) = match ctx.fx.geometry.as_ref() {
                Some(geom) => {
                    let menu_hover = geom
                        .menu_xy
                        .and_then(|(mx, my)| panel::menu_hit(mx, my, &geom.metrics, x, y));
                    // 菜单项压住的行不参与悬停,避免菜单下还有一块高亮跟着动。
                    let hit = if menu_hover.is_some() {
                        panel::Hit::Nothing
                    } else {
                        panel::hit_test(&geom.spec, &geom.metrics, ctx.font.as_ref(), x, y)
                    };
                    let item = match hit {
                        panel::Hit::Item(index) => Some(index),
                        _ => None,
                    };
                    let pager = match hit {
                        panel::Hit::PrevPage | panel::Hit::NextPage => Some(hit),
                        _ => None,
                    };
                    (item, pager, menu_hover)
                }
                None => (None, None, None),
            };
            if item != ctx.fx.hover || pager != ctx.fx.pager || menu_hover != ctx.fx.menu_hover {
                ctx.fx.hover = item;
                ctx.fx.pager = pager;
                ctx.fx.menu_hover = menu_hover;
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
                        match panel::hit_test(&geom.spec, &geom.metrics, ctx.font.as_ref(), x, y) {
                            panel::Hit::Item(slot) => Click::Pick(slot),
                            panel::Hit::PrevPage => Click::Page(-1),
                            panel::Hit::NextPage => Click::Page(1),
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
                Click::Page(delta) => {
                    let _ = crate::imp::handle(&ctx.state, Request::Page { session, delta });
                }
                Click::Nothing => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            if !interactive(&ctx.state) {
                return LRESULT(0);
            }
            let target = match ctx.fx.geometry.as_ref() {
                Some(geom) => {
                    match panel::hit_test(&geom.spec, &geom.metrics, ctx.font.as_ref(), x, y) {
                        panel::Hit::Item(slot) => geom.spec.items.get(slot).map(|item| {
                            let (_, top) =
                                menu_origin(slot, &geom.spec, &geom.metrics, ctx.font.as_ref());
                            (slot, top, item.text.clone())
                        }),
                        _ => None,
                    }
                }
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
            // 高 16 位是有符号滚轮增量;向上翻上一页,向下翻下一页。
            let delta = (wparam.0 >> 16) as u16 as i16;
            if delta != 0 && interactive(&ctx.state) {
                let session = ctx.state.active.load(Ordering::Relaxed);
                let _ = crate::imp::handle(
                    &ctx.state,
                    Request::Page {
                        session,
                        delta: if delta > 0 { -1 } else { 1 },
                    },
                );
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 鼠标状态变了要重画;淡出窗口不接鼠标,内容与位置都不动。
fn redraw(ctx: &mut WinCtx) {
    if ctx.fx.hide.is_some() {
        return;
    }
    let Some(view) = ctx.fx.last.clone() else {
        return;
    };
    crate::ui_skia::draw_view(ctx, &view);
}
