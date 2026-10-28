//! Skia 渲染核心 E2E:光栅出帧、SkSL 效果契约与坏效果回退。
//!
//! 先列失败方式:
//! 1. 面板底色/预编辑条画错颜色或圆角把角上像素染成底色。
//! 2. SkSL 模板编译失败,runtime effect 在光栅后端不可用。
//! 3. uniform 打包错位:效果读到的 u_accent/u_size 不是契约值。
//! 4. 坏 SkSL 没有返回错误,或错误被静默吞掉。
//! 5. 换行/竖排行数与 server 的窗口高度公式不一致。

use lueying_render::effect::{self, Uniforms};
use lueying_render::panel::{self, Item, Layout, Metrics, Palette, PanelSpec, PreeditBar};
use skia_safe::{ImageInfo, Paint, Surface};

fn push(checks: &mut Vec<(&'static str, bool, String)>, name: &'static str, ok: bool, note: String) {
    checks.push((name, ok, note));
}

/// N32 小端内存字节序是 BGRA,读回 (B, G, R)。
fn read_bgr(surface: &mut Surface, x: i32, y: i32) -> Option<(u8, u8, u8)> {
    let (w, h) = (surface.width(), surface.height());
    let info = ImageInfo::new_n32_premul((w, h), None);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    if !surface.read_pixels(&info, &mut buf, info.min_row_bytes(), (0, 0)) {
        return None;
    }
    let at = ((y * w + x) * 4) as usize;
    Some((buf[at], buf[at + 1], buf[at + 2]))
}

fn rgb(rgb: u32) -> (u8, u8, u8) {
    (
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

fn spec() -> PanelSpec {
    PanelSpec {
        width: 360.0,
        max_text_width: 300.0,
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

fn palette() -> Palette {
    Palette {
        bg: 0xffffff,
        surface_alt: 0xf2f2f2,
        border: 0xdddddd,
        text: 0x111111,
        text_secondary: 0x444444,
        text_tertiary: 0x888888,
        accent: 0x2f6fed,
        accent_text: 0x2459c9,
        selected_bg: 0xeaf1ff,
        selected_text: 0x11224d,
    }
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();

    // 面板底色与预编辑条:(5,60) 在候选区背景上,(5,10) 在拼音条上。
    let mut surface = Surface::new_raster_n32_premul((360, 120)).expect("raster surface");
    let metrics = Metrics::standard();
    panel::draw(
        surface.canvas(),
        &spec(),
        &palette(),
        &metrics,
        None,
    );
    let expected_bg = rgb(palette().bg);
    let expected_bar = rgb(palette().surface_alt);
    match read_bgr(&mut surface, 5, 60) {
        Some(pixel) => push(
            &mut checks,
            "面板底色",
            pixel == expected_bg,
            format!("{pixel:?} 期望 {expected_bg:?}"),
        ),
        None => push(&mut checks, "面板底色", false, "像素读回失败".into()),
    }
    match read_bgr(&mut surface, 5, 10) {
        Some(pixel) => push(
            &mut checks,
            "预编辑条底色",
            pixel == expected_bar,
            format!("{pixel:?} 期望 {expected_bar:?}"),
        ),
        None => push(&mut checks, "预编辑条底色", false, "像素读回失败".into()),
    }

    // SkSL 契约:模板可编译、uniform 打包按名字对位、坏效果报错。
    match effect::compile_background(effect::BACKGROUND_TEMPLATE) {
        Ok(rt) => {
            let mut small = Surface::new_raster_n32_premul((32, 32)).expect("raster surface");
            let uniforms = Uniforms {
                size: [32.0, 32.0],
                time: 0.0,
                anim: 1.0,
                accent: [0.0, 0.0, 1.0, 1.0],
            };
            let mut paint = Paint::default();
            let mut shader_ok = false;
            if let Some(shader) = effect::background_shader(&rt, &uniforms) {
                paint.set_shader(shader);
                small.canvas().draw_paint(&paint);
                shader_ok = true;
            }
            let blue = read_bgr(&mut small, 16, 16).map(|(b, _, _)| b).unwrap_or(0);
            // 模板在中心处 accent × (0.10 + 0.04×smoothstep) ≈ 0.13 × 255 ≈ 33。
            push(
                &mut checks,
                "SkSL 背景效果",
                shader_ok && (20..=44).contains(&blue),
                format!("shader_ok={shader_ok} B={blue}"),
            );
        }
        Err(e) => push(&mut checks, "SkSL 背景效果", false, e),
    }
    let bad = effect::compile_background("half4 main(float2 p) { return nonexistent; }");
    push(
        &mut checks,
        "坏效果回退",
        bad.is_err(),
        match bad {
            Ok(_) => "坏 SkSL 竟然编译通过".into(),
            Err(_) => "编译错误正常返回".into(),
        },
    );

    // 布局:两个短候选一行;长候选换行;竖排一条一行,行数进窗口高度公式。
    let rows = panel::layout_rows(&spec(), &metrics, None);
    push(
        &mut checks,
        "横排短候选单行",
        rows == 1,
        format!("rows={rows}"),
    );
    let mut long = spec();
    long.items = (0..9)
        .map(|i| Item {
            text: format!("候选{i}是很长的整句词条内容"),
            hint: String::new(),
            selected: false,
        })
        .collect();
    let long_rows = panel::layout_rows(&long, &metrics, None);
    push(
        &mut checks,
        "横排长候选换行",
        long_rows >= 3,
        format!("rows={long_rows}"),
    );
    let mut column = spec();
    column.layout = Layout::Column;
    let column_rows = panel::layout_rows(&column, &metrics, None);
    push(
        &mut checks,
        "竖排一行一条",
        column_rows == column.items.len(),
        format!("rows={column_rows}"),
    );
    let height = panel::panel_height(&spec(), &metrics, None);
    push(
        &mut checks,
        "窗口高度公式",
        (height - (32.0 + 4.0 + 34.0 + 6.0)).abs() < f32::EPSILON,
        format!("height={height}"),
    );

    report.push_str("\n## Skia 渲染核心\n\n| 检查 | 结果 | 说明 |\n|---|---|---|\n");
    let mut failures = 0;
    for (name, ok, note) in checks {
        if !ok {
            failures += 1;
        }
        report.push_str(&format!(
            "| {name} | {} | {note} |\n",
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    failures
}
