//! Skia 候选窗自检:建窗、Ganesh-D3D12 出帧、读回像素断言。
//!
//! 与 Linux 光栅 E2E 同一套期望值(无字体估算路径),失败由 win-e2e
//! 断言;没有桌面会话或合成交换链不可用时退回离屏渲染,像素期望不变。

use lueying_render::panel::{self, Item, Layout, Metrics, Palette, PanelSpec, PreeditBar};
use lueying_server::theme;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

use crate::ui_skia::{create_window, palette_of};
use crate::ui_skia_gpu::{Frame, FrameTarget, Offscreen, Pipeline, rgb};

/// CI 自检:建窗、出帧、读回像素断言,结论写日志。
pub(super) fn selftest() {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // 固定 100% 缩放与无字体估算路径,像素期望与 Linux E2E 完全一致。
    let metrics = Metrics::standard();
    let spec = demo_spec();
    let width = panel::panel_width(&spec, &metrics, None);
    let height = panel::panel_height(&spec, &metrics, None);
    let palette = palette_of(theme::colors_with(theme::Appearance::Light, 0));
    let (mode, outcome) = match window_target(width as u32, height as u32) {
        Ok((mut pipe, hwnd)) => {
            let outcome = check_pixels(&mut pipe, &spec, &palette, &metrics);
            // 先放管线再销窗:DComp 目标还指着这个 HWND。
            drop(pipe);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            ("合成交换链", outcome)
        }
        Err(e) => {
            // 桌面会话缺失或合成交换链建不起来时,退回离屏渲染验证出帧。
            println!("SKIA 自检降级:合成交换链不可用({e}),改走离屏渲染");
            match Offscreen::new(width as u32, height as u32) {
                Ok(mut target) => (
                    "离屏渲染",
                    check_pixels(&mut target, &spec, &palette, &metrics),
                ),
                Err(e) => {
                    println!("SKIA 自检失败:离屏渲染建管线 {e}");
                    return;
                }
            }
        }
    };
    match outcome {
        Ok(()) => println!(
            "SKIA 自检通过:{mode}帧读回一致({}x{})",
            width as u32, height as u32
        ),
        Err(e) => println!("SKIA 自检失败:{mode} {e}"),
    }
}

/// 建窗 + DComp 交换链;失败时把原因带回去,自检据此决定是否降级。
fn window_target(w: u32, h: u32) -> Result<(Pipeline, HWND), String> {
    let hwnd = create_window().map_err(|e| format!("建窗 {e}"))?;
    let mut pipe = match Pipeline::new(hwnd) {
        Ok(pipe) => pipe,
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(e);
        }
    };
    if let Err(e) = pipe.rebuild_surfaces(w, h) {
        drop(pipe);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return Err(e);
    }
    Ok((pipe, hwnd))
}

/// 两帧的像素断言:静态面板的底色与拼音条,再验悬停与菜单的叠加顺序。
/// 窗口路径与离屏路径共用同一套期望值。
fn check_pixels(
    target: &mut dyn FrameTarget,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
) -> Result<(), String> {
    // 第一帧:静态面板,读回底色与拼音条。
    target.draw(Frame {
        spec,
        palette,
        metrics,
        font: None,
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: None,
        pager: None,
        menu: None,
        menu_hover: None,
        effects: panel::Effects::default(),
    })?;
    let expected_bg = rgb(palette.bg);
    let expected_bar = rgb(palette.surface_alt);
    let bg = target.pixel(5, 60).ok_or("像素读回失败(背景)")?;
    if bg != expected_bg {
        return Err(format!("背景像素 {bg:?} 期望 {expected_bg:?}"));
    }
    let bar = target.pixel(5, 10).ok_or("像素读回失败(拼音条)")?;
    if bar != expected_bar {
        return Err(format!("拼音条像素 {bar:?} 期望 {expected_bar:?}"));
    }
    // 第二帧:第二行悬停 + 菜单压住它左半,验证叠加顺序与菜单底色。
    let rects = panel::item_rects(spec, metrics, None);
    let hover_at = ((rects[1].right - 6.0) as i32, (rects[1].top + 6.0) as i32);
    let covered_at = ((rects[1].left + 6.0) as i32, (rects[1].top + 6.0) as i32);
    target.draw(Frame {
        spec,
        palette,
        metrics,
        font: None,
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: Some(1),
        pager: None,
        menu: Some((8.0, 40.0)),
        menu_hover: None,
        effects: panel::Effects::default(),
    })?;
    let hover = target
        .pixel(hover_at.0, hover_at.1)
        .ok_or("像素读回失败(悬停)")?;
    let expected_hover = rgb(palette.hover);
    if hover != expected_hover {
        return Err(format!("悬停像素 {hover:?} 期望 {expected_hover:?}"));
    }
    let covered = target
        .pixel(covered_at.0, covered_at.1)
        .ok_or("像素读回失败(菜单)")?;
    if covered != expected_bg {
        return Err(format!("菜单像素 {covered:?} 期望 {expected_bg:?}"));
    }
    Ok(())
}

/// 自检用的固定面板;内容与 Linux E2E 的 `render::spec()` 一致。
fn demo_spec() -> PanelSpec {
    PanelSpec {
        width: 360.0,
        max_text_width: 490.0,
        preedit: PreeditBar {
            text: "nihao".into(),
            page_label: "1/2".into(),
        },
        items: vec![
            Item {
                text: "你好".into(),
                hint: String::new(),
                selected: true,
            },
            Item {
                text: "你号".into(),
                hint: String::new(),
                selected: false,
            },
        ],
        layout: Layout::Wrap,
    }
}
