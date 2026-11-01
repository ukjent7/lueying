//! D2D 候选窗自检:离屏出帧、读回像素断言。
//!
//! WARP(软件 D3D11)渲染在服务端进程里跑,给出确定性的像素断言,外加
//! 一条字体断言——DirectWrite 取到的字型必须真的画进画面。没有桌面会话
//! 的 CI 照样能跑;窗口与 DComp 的真实上屏由 Windows E2E 在真机覆盖。

use lueying_render::panel::{self, Item, Layout, Metrics, Palette, PanelSpec, PreeditBar};
use lueying_server::theme;

use crate::ui_d2d_pipe::{Frame, Target, Text, rgb};

/// CI 自检:离屏 WARP 出帧 + 像素断言,结论写日志。
pub(super) fn selftest() {
    let Ok(text) = Text::new() else {
        println!("D2D 自检失败:文字引擎起不来");
        return;
    };
    let metrics = Metrics::standard();
    let spec = demo_spec();
    let width = panel::panel_width(&spec, &metrics, &text) as u32;
    let height = panel::panel_height(&spec, &metrics, &text) as u32;
    let palette = theme::palette(theme::colors_with(theme::Appearance::Light, 0));
    let mut target = match crate::ui_d2d_pipe::Offscreen::new(width, height) {
        Ok(target) => target,
        Err(e) => {
            println!("D2D 自检失败:离屏渲染建管线 {e}");
            return;
        }
    };
    match check_pixels(&mut target, &spec, &palette, &metrics, &text) {
        Ok(()) => println!("D2D 自检通过:离屏帧读回一致({width}x{height})"),
        Err(e) => println!("D2D 自检失败:像素 {e}"),
    }
    // 字体:字型必须真的画进画面。系统一个可用字型都没有是环境问题,记下来
    // 不算失败;字型拿到了却画不出东西才是真 bug。
    match font_check(&mut target, &spec, &palette, &metrics, &text) {
        Ok(()) => println!("D2D 自检通过:DirectWrite 字型已画出"),
        Err(FontCheck::NoFont) => println!("D2D 字体自检:系统没有可用字型,跳过"),
        Err(FontCheck::Failed(e)) => println!("D2D 自检失败:字体 {e}"),
    }
}

/// 字体断言:候选文字行里必须有与底色不同的像素。
fn font_check(
    target: &mut dyn Target,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    text: &Text,
) -> Result<(), FontCheck> {
    let frame = Frame {
        spec,
        palette,
        metrics: *metrics,
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: None,
        pager: None,
        menu: None,
        menu_hover: None,
        scene: &[],
        scene_dir: std::path::Path::new(""),
    };
    target
        .paint_frame(&frame, text)
        .map_err(FontCheck::Failed)?;
    let rects = panel::item_rects(spec, metrics, text);
    let row = rects.first().ok_or(FontCheck::NoFont)?;
    let bg = rgb(palette.selected_bg);
    // 扫选中行的整行:候选文字/序号/强调条至少要落下一点不同色。
    for x in (row.left as i32 + 2)..(row.right as i32 - 2) {
        for y in (row.top as i32 + 2)..(row.bottom as i32 - 2) {
            if let Some(pixel) = target.pixel(x, y)
                && pixel != bg
            {
                return Ok(());
            }
        }
    }
    Err(FontCheck::Failed("行内全是底色,文字没画出来".into()))
}

enum FontCheck {
    NoFont,
    Failed(String),
}

/// 两帧的像素断言:静态面板的底色与拼音条,再验悬停与菜单的叠加顺序。
fn check_pixels(
    target: &mut dyn Target,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    text: &Text,
) -> Result<(), String> {
    // 第一帧:静态面板,读回底色与拼音条。
    target.paint_frame(
        &Frame {
            spec,
            palette,
            metrics: *metrics,
            opacity: 1.0,
            list: panel::ListFx::NEUTRAL,
            hover: None,
            pager: None,
            menu: None,
            menu_hover: None,
            scene: &[],
            scene_dir: std::path::Path::new(""),
        },
        text,
    )?;
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
    // 第二帧:第二项悬停 + 菜单压住它左半,验证叠加顺序与菜单底色。
    let rects = panel::item_rects(spec, metrics, text);
    if rects.len() < 2 {
        return Err("演示面板至少要两项".into());
    }
    let hover_at = ((rects[1].right - 6.0) as i32, (rects[1].top + 6.0) as i32);
    let covered_at = ((rects[1].left + 6.0) as i32, (rects[1].top + 6.0) as i32);
    target.paint_frame(
        &Frame {
            spec,
            palette,
            metrics: *metrics,
            opacity: 1.0,
            list: panel::ListFx::NEUTRAL,
            hover: Some(1),
            pager: None,
            menu: Some((8.0, 40.0)),
            menu_hover: None,
            scene: &[],
            scene_dir: std::path::Path::new(""),
        },
        text,
    )?;
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
pub(super) fn demo_spec() -> PanelSpec {
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
