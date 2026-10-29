//! Skia 渲染核心 E2E:光栅出帧、SkSL 效果契约与坏效果回退。
//!
//! 先列失败方式:
//! 1. 面板底色/预编辑条画错颜色或圆角把角上像素染成底色。
//! 2. SkSL 模板编译失败,runtime effect 在光栅后端不可用。
//! 3. uniform 打包错位:效果读到的 u_accent/u_size 不是契约值。
//! 4. 坏 SkSL 没有返回错误,或错误被静默吞掉。
//! 5. 换行/竖排行数与 server 的窗口高度公式不一致。
//! 6. 用户效果盖过文字:遮罩没把对比度守住。

use std::time::{Duration, Instant};

use lueying_render::badge;
use lueying_render::effect::{self, Uniforms};
use lueying_render::panel::{self, Item, Layout, Metrics, Palette, PanelSpec, PreeditBar};
use lueying_render::ui;
use skia_safe::{ImageInfo, Paint, Surface, surfaces};

fn push(
    checks: &mut Vec<(&'static str, bool, String)>,
    name: &'static str,
    ok: bool,
    note: String,
) {
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

/// 读回一个像素的 (B, G, R, A);N32 预乘,透明处的 alpha 是 0。
fn read_bgra(surface: &mut Surface, x: i32, y: i32) -> Option<(u8, u8, u8, u8)> {
    let (w, h) = (surface.width(), surface.height());
    let info = ImageInfo::new_n32_premul((w, h), None);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    if !surface.read_pixels(&info, &mut buf, info.min_row_bytes(), (0, 0)) {
        return None;
    }
    let at = ((y * w + x) * 4) as usize;
    Some((buf[at], buf[at + 1], buf[at + 2], buf[at + 3]))
}

/// 0xRRGGBB 转成与 [`read_bgr`] 同序的 (B, G, R),比较时才不会错位。
fn rgb(rgb: u32) -> (u8, u8, u8) {
    (
        (rgb & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        ((rgb >> 16) & 0xff) as u8,
    )
}

/// sRGB 通道值转线性值。
fn linearize(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// 像素(读回是 BGR)的相对亮度;与渲染层的遮罩算法各算各的,互为验证。
fn luminance(pixel: (u8, u8, u8)) -> f32 {
    let (b, g, r) = pixel;
    let channel = |value: u8| linearize(value as f32 / 255.0);
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// 两个像素的对比度。
fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x >= y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
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
        window_bg: 0xf3f4f7,
        surface_alt: 0xf2f2f2,
        border: 0xdddddd,
        border_strong: 0xcfd2d9,
        text: 0x111111,
        text_secondary: 0x444444,
        text_tertiary: 0x6a6e77,
        accent: 0x2f6fed,
        on_accent: 0xffffff,
        segment: 0xffffff,
        off_bg: 0xd9dce3,
        knob_off: 0xffffff,
        danger: 0xb3261e,
        danger_soft: 0xfdecea,
        accent_text: 0x2459c9,
        selected_bg: 0xeaf1ff,
        selected_text: 0x11224d,
        hover: 0xf0f2f6,
    }
}

/// 矩形中心横坐标。
fn center_x(rect: skia_safe::Rect) -> f32 {
    (rect.left + rect.right) / 2.0
}

/// 矩形中心纵坐标。
fn center_y(rect: skia_safe::Rect) -> f32 {
    (rect.top + rect.bottom) / 2.0
}

/// 设置窗的样例页面:一个分组,三种控件各来一个。
fn settings_page() -> ui::Page {
    ui::Page {
        title: "掠影".into(),
        subtitle: "拼音输入法 · v0.1.0".into(),
        header_button: Some(ui::Button {
            id: ui::Id::new("update", 0),
            text: "检查更新".into(),
            kind: ui::ButtonKind::Ghost,
            enabled: true,
        }),
        groups: vec![ui::Group {
            title: "输入".into(),
            rows: vec![
                ui::Row {
                    label: "辅助码".into(),
                    detail: "拼音后按 ` 再打 1–2 位五笔码".into(),
                    control: ui::Control::Toggle {
                        id: ui::Id::new("aux", 0),
                        on: true,
                    },
                },
                ui::Row {
                    label: "输入方案".into(),
                    detail: "全拼或双拼,共享词库".into(),
                    control: ui::Control::Segment {
                        id: ui::Id::new("scheme", 0),
                        options: vec![
                            ui::SegmentOption::new("全拼"),
                            ui::SegmentOption::new("双拼"),
                        ],
                        selected: 0,
                        width: 44.0,
                    },
                },
                ui::Row {
                    label: "自定义短语".into(),
                    detail: "编辑 phrases.txt,保存后重载".into(),
                    control: ui::Control::Buttons {
                        items: vec![ui::Button {
                            id: ui::Id::new("open-phrases", 0),
                            text: "编辑".into(),
                            kind: ui::ButtonKind::Secondary,
                            enabled: true,
                        }],
                    },
                },
                ui::Row {
                    label: "用户效果".into(),
                    detail: "SkSL 装饰层,放进去即生效".into(),
                    control: ui::Control::ButtonsToggle {
                        items: vec![ui::Button {
                            id: ui::Id::new("open-effects", 0),
                            text: "目录".into(),
                            kind: ui::ButtonKind::Secondary,
                            enabled: true,
                        }],
                        toggle: ui::Id::new("effects", 0),
                        toggle_on: true,
                    },
                },
            ],
        }],
        notice: "修改后点保存生效".into(),
        notice_tint: 0x5f636b,
        footer_buttons: vec![
            ui::Button {
                id: ui::Id::new("close", 0),
                text: "关闭".into(),
                kind: ui::ButtonKind::Secondary,
                enabled: true,
            },
            ui::Button {
                id: ui::Id::new("save", 0),
                text: "保存".into(),
                kind: ui::ButtonKind::Primary,
                enabled: true,
            },
        ],
    }
}

/// 设置窗用的字型;系统没有字型时退回默认字型(只影响宽度测量)。
fn ui_font() -> skia_safe::Font {
    let mgr = skia_safe::FontMgr::new();
    match mgr.legacy_make_typeface(None, skia_safe::FontStyle::normal()) {
        Some(face) => skia_safe::Font::from_typeface(face, 17.0),
        None => skia_safe::Font::default(),
    }
}

pub fn run(report: &mut String) -> usize {
    let mut checks = Vec::new();

    // 面板底色与预编辑条:(5,60) 在候选区背景上,(5,10) 在拼音条上。
    let mut surface = surfaces::raster_n32_premul((360, 120)).expect("raster surface");
    let metrics = Metrics::standard();
    panel::draw(surface.canvas(), &spec(), &palette(), &metrics, None);
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
            let mut small = surfaces::raster_n32_premul((32, 32)).expect("raster surface");
            let uniforms = Uniforms {
                size: [32.0, 32.0],
                time: 0.0,
                anim: 1.0,
                accent: [0.0, 0.0, 1.0, 1.0],
            };
            let mut paint = Paint::default();
            let mut shader_ok = false;
            if let Some(shader) = effect::make_shader(&rt, &uniforms) {
                paint.set_shader(shader);
                small.canvas().draw_paint(&paint);
                shader_ok = true;
            }
            let blue = read_bgr(&mut small, 16, 16).map(|(b, _, _)| b).unwrap_or(0);
            // 模板在中心处 accent × (0.55 + 0.35×smoothstep) ≈ 0.9 × 255 ≈ 230。
            push(
                &mut checks,
                "SkSL 背景效果",
                shader_ok && (200..=255).contains(&blue),
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

    // 用户效果:纯黑效果盖住面板底,遮罩要把它拉回来并守住文字对比度。
    let solid = "half4 main(float2 p) { return half4(0.0, 0.0, 0.0, 1.0); }";
    match effect::compile_background(solid) {
        Ok(rt) => {
            let uniforms = Uniforms {
                size: [360.0, 120.0],
                time: 0.0,
                anim: 1.0,
                accent: [0.0, 0.0, 0.0, 0.0],
            };
            let shader = effect::make_shader(&rt, &uniforms);
            let spec = spec();
            let palette = palette();
            let (width, height) = (
                panel::panel_width(&spec, &metrics, None),
                panel::panel_height(&spec, &metrics, None),
            );
            let mut surface = surfaces::raster_n32_premul((360, 120)).expect("raster surface");
            panel::draw_chrome(
                surface.canvas(),
                width,
                height,
                &spec,
                &palette,
                &metrics,
                None,
                None,
                panel::Effects {
                    background: shader.as_ref(),
                    ..panel::Effects::default()
                },
            );
            match read_bgr(&mut surface, 5, 60) {
                Some(pixel) => {
                    let plain = rgb(palette.bg);
                    let body = contrast(pixel, rgb(palette.text));
                    let weak = contrast(pixel, rgb(palette.text_tertiary));
                    push(
                        &mut checks,
                        "效果槽位出图",
                        // 效果确实画上去了(与素色不同),又被遮罩拉回可读区间。
                        pixel != plain && pixel.0 >= 128 && body >= 4.5 && weak >= 3.0,
                        format!("{pixel:?} 素色 {plain:?} 正文 {body:.2} 弱文字 {weak:.2}"),
                    );
                }
                None => push(&mut checks, "效果槽位出图", false, "像素读回失败".into()),
            }
        }
        Err(e) => push(&mut checks, "效果槽位出图", false, e),
    }

    // 遮罩不透明度:按渲染层同样的口径解出来(正文与弱文字取更严的一档),
    // 再把不透明度量化成 8 位 alpha,两端最坏情况都要守住对比度。
    let alpha = panel::scrim_alpha(palette().bg, palette().text, panel::CONTRAST_TEXT).max(
        panel::scrim_alpha(palette().bg, palette().text_tertiary, panel::CONTRAST_WEAK),
    );
    let byte = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = byte as f32 / 255.0;
    let base = palette().bg;
    // 遮罩是底色按 alpha 压在效果上;Skia 在 sRGB 空间里做 src-over。
    // 效果能画纯黑,也能画纯白,两端都要够。
    let over = |effect: f32| -> (u8, u8, u8) {
        let channel = |shift: u32| ((base >> shift) & 0xff) as f32;
        (
            (effect * (1.0 - a) + channel(0) * a).round() as u8,
            (effect * (1.0 - a) + channel(8) * a).round() as u8,
            (effect * (1.0 - a) + channel(16) * a).round() as u8,
        )
    };
    let dark = over(0.0);
    let light = over(255.0);
    let body = contrast(dark, rgb(palette().text)).min(contrast(light, rgb(palette().text)));
    let weak = contrast(dark, rgb(palette().text_tertiary))
        .min(contrast(light, rgb(palette().text_tertiary)));
    push(
        &mut checks,
        "遮罩守住对比度",
        alpha > 0.0 && alpha < 1.0 && body >= 4.5 && weak >= 3.0,
        format!("alpha={alpha:.3} 暗端 {dark:?} 亮端 {light:?} 正文 {body:.2} 弱文字 {weak:.2}"),
    );

    // 坏文件只废掉自己的槽位:好模板、坏 SkSL、版本过高三份文件同放一个目录。
    let dir = std::env::temp_dir().join(format!("lueying-effect-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let prepared = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(dir.join("background.sksl"), effect::BACKGROUND_TEMPLATE))
        .and_then(|()| {
            std::fs::write(
                dir.join("highlight.sksl"),
                "half4 main(float2 p) { return nonexistent; }",
            )
        })
        .and_then(|()| {
            std::fs::write(
                dir.join("menu.sksl"),
                "// effect-api: 99\nhalf4 main(float2 p) { return half4(0.0); }",
            )
        });
    if prepared.is_err() {
        push(
            &mut checks,
            "坏效果只废一个槽位",
            false,
            "临时目录写不了".into(),
        );
    } else {
        let set = effect::EffectSet::load_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        push(
            &mut checks,
            "坏效果只废一个槽位",
            set.has(effect::Slot::Background)
                && !set.has(effect::Slot::Highlight)
                && !set.has(effect::Slot::Menu)
                && set.errors().len() == 2,
            format!(
                "背景={} 选中={} 菜单={} 错误 {:?}",
                set.has(effect::Slot::Background),
                set.has(effect::Slot::Highlight),
                set.has(effect::Slot::Menu),
                set.errors()
            ),
        );
        push(
            &mut checks,
            "契约版本过高拒绝加载",
            set.errors().iter().any(|e| e.contains("契约版本")),
            format!("{:?}", set.errors()),
        );
    }

    // 运行期脚本给的源码:好源码编译,坏源码只废自己,与目录加载同一套错误。
    let set = effect::EffectSet::from_sources([
        (effect::Slot::Background, effect::BACKGROUND_TEMPLATE),
        (
            effect::Slot::Menu,
            "half4 main(float2 p) { return nonexistent; }",
        ),
    ]);
    push(
        &mut checks,
        "源码效果只废坏槽位",
        set.has(effect::Slot::Background)
            && !set.has(effect::Slot::Menu)
            && !set.has(effect::Slot::Highlight)
            && set.errors().len() == 1,
        format!(
            "背景={} 菜单={} 错误 {:?}",
            set.has(effect::Slot::Background),
            set.has(effect::Slot::Menu),
            set.errors()
        ),
    );

    // 帧预算:连续超预算就摘掉效果,冷却期内不放行,冷却到期再给一次机会。
    let mut guard = effect::BudgetGuard::new(effect::Budget::default());
    let now = Instant::now();
    guard.observe(Duration::from_millis(20), now);
    guard.observe(Duration::from_millis(20), now);
    let blocked = !guard.allows(now);
    let after = now + Duration::from_millis(2500);
    let allowed = guard.allows(after);
    push(
        &mut checks,
        "帧预算摘效果",
        blocked && allowed,
        format!("冷却内 blocked={blocked} 冷却后 allowed={allowed}"),
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
        // 拼音条 32 + 列表上边距 4 + 一行 30 + 底边距 6;与 GPUI 路径同公式。
        (height - (32.0 + 4.0 + 30.0 + 6.0)).abs() < f32::EPSILON,
        format!("height={height}"),
    );

    // 命中测试与绘制共用布局:所指即所画,窗口层不用再写一份命中判定。
    let hit_spec = spec();
    let hit_metrics = Metrics::standard();
    let rects = panel::item_rects(&hit_spec, &hit_metrics, None);
    let first = rects[0];
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        None,
        first.left + 2.0,
        first.top + 2.0,
    );
    push(
        &mut checks,
        "命中候选行",
        hit == panel::Hit::Item(0),
        format!("{hit:?}"),
    );
    let hit = panel::hit_test(&hit_spec, &hit_metrics, None, 5.0, 10.0);
    push(
        &mut checks,
        "命中拼音条空白",
        hit == panel::Hit::Nothing,
        format!("{hit:?}"),
    );
    let hit_width = panel::panel_width(&hit_spec, &hit_metrics, None);
    let (prev, _, next) = panel::pager_rects(hit_width, &hit_metrics);
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        None,
        (prev.left + prev.right) * 0.5,
        (prev.top + prev.bottom) * 0.5,
    );
    push(
        &mut checks,
        "命中上一页箭头",
        hit == panel::Hit::PrevPage,
        format!("{hit:?}"),
    );
    let hit = panel::hit_test(
        &hit_spec,
        &hit_metrics,
        None,
        (next.left + next.right) * 0.5,
        (next.top + next.bottom) * 0.5,
    );
    push(
        &mut checks,
        "命中下一页箭头",
        hit == panel::Hit::NextPage,
        format!("{hit:?}"),
    );

    // 右键菜单:两行 26px,行间空隙与菜单外都不是菜单项。
    push(
        &mut checks,
        "菜单首行",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 17.0) == Some(0),
        format!("{:?}", panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 17.0)),
    );
    push(
        &mut checks,
        "菜单次行",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 45.0) == Some(1),
        format!("{:?}", panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 45.0)),
    );
    push(
        &mut checks,
        "菜单行间隙与外侧",
        panel::menu_hit(0.0, 0.0, &hit_metrics, 10.0, 31.0).is_none()
            && panel::menu_hit(0.0, 0.0, &hit_metrics, 200.0, 17.0).is_none(),
        "间隙与 116px 外都落空".into(),
    );

    // 悬停底色:非选中行画 hover 色,绘制与命中同一份矩形。
    let mut hover_surface = surfaces::raster_n32_premul((360, 120)).expect("raster surface");
    let (hw, hh) = (
        panel::panel_width(&hit_spec, &hit_metrics, None),
        panel::panel_height(&hit_spec, &hit_metrics, None),
    );
    panel::draw_chrome(
        hover_surface.canvas(),
        hw,
        hh,
        &hit_spec,
        &palette(),
        &hit_metrics,
        None,
        None,
        panel::Effects::default(),
    );
    panel::draw_items(
        hover_surface.canvas(),
        hw,
        hh,
        &hit_spec,
        &palette(),
        &hit_metrics,
        None,
        panel::ListFx::NEUTRAL,
        Some(1),
        panel::Effects::default(),
    );
    let second = rects[1];
    let expected_hover = rgb(palette().hover);
    match read_bgr(
        &mut hover_surface,
        second.left as i32 + 6,
        second.top as i32 + 6,
    ) {
        Some(pixel) => push(
            &mut checks,
            "悬停底色",
            pixel == expected_hover,
            format!("{pixel:?} 期望 {expected_hover:?}"),
        ),
        None => push(&mut checks, "悬停底色", false, "像素读回失败".into()),
    }

    // 菜单绘制:首行悬停给 hover 底色,次行保持菜单底色;与 menu_hit 同一套行几何。
    let mut menu_surface = surfaces::raster_n32_premul((160, 80)).expect("raster surface");
    panel::draw_menu(
        menu_surface.canvas(),
        0.0,
        0.0,
        &palette(),
        &hit_metrics,
        None,
        Some(0),
        panel::Effects::default(),
    );
    let expected_menu_hover = rgb(palette().hover);
    match read_bgr(&mut menu_surface, 10, 17) {
        Some(pixel) => push(
            &mut checks,
            "菜单悬停底色",
            pixel == expected_menu_hover,
            format!("{pixel:?} 期望 {expected_menu_hover:?}"),
        ),
        None => push(&mut checks, "菜单悬停底色", false, "像素读回失败".into()),
    }
    let expected_menu_bg = rgb(palette().bg);
    match read_bgr(&mut menu_surface, 10, 45) {
        Some(pixel) => push(
            &mut checks,
            "菜单底色",
            pixel == expected_menu_bg,
            format!("{pixel:?} 期望 {expected_menu_bg:?}"),
        ),
        None => push(&mut checks, "菜单底色", false, "像素读回失败".into()),
    }

    // 翻页箭头悬停:位置由 pager_rects 决定,悬停时垫一层描边色底。
    let mut pager_surface = surfaces::raster_n32_premul((360, 120)).expect("raster surface");
    panel::draw_chrome(
        pager_surface.canvas(),
        hw,
        hh,
        &hit_spec,
        &palette(),
        &hit_metrics,
        None,
        Some(panel::Hit::PrevPage),
        panel::Effects::default(),
    );
    let (prev, _, _) = panel::pager_rects(hw, &hit_metrics);
    let expected_pager = rgb(palette().border);
    let (px, py) = (
        ((prev.left + prev.right) * 0.5) as i32,
        ((prev.top + prev.bottom) * 0.5) as i32,
    );
    match read_bgr(&mut pager_surface, px, py) {
        Some(pixel) => push(
            &mut checks,
            "箭头悬停底色",
            pixel == expected_pager,
            format!("{pixel:?} 期望 {expected_pager:?}"),
        ),
        None => push(&mut checks, "箭头悬停底色", false, "像素读回失败".into()),
    }

    // 模式徽标:卡片底色、圆角外的透明,以及字型有没有真的画出来。
    let (bw, bh) = (badge::WIDTH as i32, badge::HEIGHT as i32);
    let plain_badge = |surface: &mut Surface, font: Option<&skia_safe::Font>| {
        badge::draw(
            surface.canvas(),
            &badge::BadgeSpec {
                label: "中",
                palette: &palette(),
                font,
                width: badge::WIDTH,
                height: badge::HEIGHT,
                scale: 1.0,
                shadow: false,
            },
        );
    };
    let mut badge_surface = surfaces::raster_n32_premul((bw, bh)).expect("raster surface");
    plain_badge(&mut badge_surface, None);
    match read_bgra(&mut badge_surface, bw / 2, bh / 2) {
        Some(pixel) => push(
            &mut checks,
            "徽标卡片底色",
            pixel.3 == 255 && (pixel.0, pixel.1, pixel.2) == rgb(palette().bg),
            format!("{pixel:?} 期望 {:?} 全不透明", rgb(palette().bg)),
        ),
        None => push(&mut checks, "徽标卡片底色", false, "像素读回失败".into()),
    }
    match read_bgra(&mut badge_surface, 0, 0) {
        Some(pixel) => push(
            &mut checks,
            "徽标圆角外透明",
            pixel.3 == 0,
            format!("{pixel:?} 期望 alpha=0"),
        ),
        None => push(&mut checks, "徽标圆角外透明", false, "像素读回失败".into()),
    }
    // 字型:系统一个可用字型都没有是环境问题,跳过;有字型却画不出东西才是 bug。
    let mgr = skia_safe::FontMgr::new();
    let families = mgr.count_families();
    match mgr.legacy_make_typeface(None, skia_safe::FontStyle::normal()) {
        Some(face) => {
            let font = skia_safe::Font::from_typeface(face, 17.0);
            let mut with_font = surfaces::raster_n32_premul((bw, bh)).expect("raster surface");
            plain_badge(&mut with_font, Some(&font));
            let mut plain = surfaces::raster_n32_premul((bw, bh)).expect("raster surface");
            plain_badge(&mut plain, None);
            let mut differs = false;
            'y: for y in 0..bh {
                for x in 0..bw {
                    if read_bgra(&mut with_font, x, y) != read_bgra(&mut plain, x, y) {
                        differs = true;
                        break 'y;
                    }
                }
            }
            // 一个字体族都没有时,字型画不出东西是环境问题,不算渲染 bug。
            let ok = differs || families == 0;
            push(
                &mut checks,
                "徽标字型出图",
                ok,
                format!("与无字型画面不同={differs} 字体族={families}"),
            );
        }
        None => push(
            &mut checks,
            "徽标字型出图",
            true,
            "系统没有可用字型,跳过".into(),
        ),
    }

    // 设置窗控件层:布局、命中与像素。
    let font = ui_font();
    let page = settings_page();
    let layout = ui::layout(&page, &font, 1.0, 650.0);
    // 命中:开关、分段格、按钮各归各位;空白处不命中。
    let toggle = layout.card_hit(ui::Id::new("aux", 0)).expect("开关矩形");
    let hit = layout.hit_card(center_x(toggle), center_y(toggle));
    push(
        &mut checks,
        "设置开关命中",
        hit == Some(ui::Id::new("aux", 0)),
        format!("{hit:?}"),
    );
    let cell = layout
        .card_hit(ui::Id::new("scheme", 1))
        .expect("分段格矩形");
    let hit = layout.hit_card(center_x(cell), center_y(cell));
    push(
        &mut checks,
        "设置分段命中",
        hit == Some(ui::Id::new("scheme", 1)),
        format!("{hit:?}"),
    );
    let button = layout
        .card_hit(ui::Id::new("open-phrases", 0))
        .expect("按钮矩形");
    let hit = layout.hit_card(center_x(button), center_y(button));
    push(
        &mut checks,
        "设置按钮命中",
        hit == Some(ui::Id::new("open-phrases", 0)),
        format!("{hit:?}"),
    );
    let hit = layout.hit_card(4.0, 4.0);
    push(
        &mut checks,
        "设置空白不命中",
        hit.is_none(),
        format!("{hit:?}"),
    );
    // 控件右对齐在行内,不出卡片(卡片坐标)。
    let aligned = (toggle.right - (layout.card.width() - 14.0)).abs() < 0.5;
    let inside = toggle.right <= layout.card.width() && toggle.left >= 0.0;
    push(
        &mut checks,
        "设置控件右对齐",
        aligned && inside,
        format!("右边距={:.1}", layout.card.right - toggle.right),
    );
    // 像素:窗口底色、开关闭合的旋钮、选中分段的底色。
    let mut ui_surface = surfaces::raster_n32_premul((460, 650)).expect("raster surface");
    ui::Painter::new(ui_surface.canvas(), &palette(), &font, 1.0).paint(&page, &layout, None, 0.0);
    let corner = read_bgra(&mut ui_surface, 2, 2);
    push(
        &mut checks,
        "设置窗底色",
        corner.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette().window_bg)),
        format!("{corner:?} 期望 {:?}", rgb(palette().window_bg)),
    );
    let knob_x = (layout.card.left + toggle.right - 3.0 - 8.0) as i32;
    let knob_y = (layout.card.top + center_y(toggle)) as i32;
    let knob = read_bgra(&mut ui_surface, knob_x, knob_y);
    push(
        &mut checks,
        "设置开关旋钮",
        knob.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette().on_accent)),
        format!("{knob:?} 期望 {:?}", rgb(palette().on_accent)),
    );
    // 选中格的底色在格子左缘(避开居中的文字);只有选中的那格是白的,
    // 没选中的格子露的是分段轨道底色。
    let selected_cell = layout
        .card_hit(ui::Id::new("scheme", 0))
        .expect("选中分段格矩形");
    let (cell_x, cell_y) = (
        (layout.card.left + selected_cell.left + 2.0) as i32,
        (layout.card.top + center_y(selected_cell)) as i32,
    );
    let cell_pixel = read_bgra(&mut ui_surface, cell_x, cell_y);
    push(
        &mut checks,
        "设置选中分段底色",
        cell_pixel.map(|p| (p.0, p.1, p.2)) == Some(rgb(palette().segment)),
        format!("{cell_pixel:?} 期望 {:?}", rgb(palette().segment)),
    );
    // 页脚贴视口底:内容比视口矮时页脚也在底上。
    let footer_bottom = layout.footer.bottom;
    push(
        &mut checks,
        "设置页脚贴底",
        (footer_bottom - (650.0 - 18.0)).abs() < 0.5,
        format!("页脚下沿={footer_bottom:.1}"),
    );
    // 视口够高时卡片不滚动。
    let tall = ui::layout(&page, &font, 1.0, 2000.0);
    push(
        &mut checks,
        "设置视口够高不滚动",
        tall.max_scroll() == 0.0,
        format!("max_scroll={:.1}", tall.max_scroll()),
    );
    // 视口矮时卡片能滚,滚动量等于多出来的高度。
    let short = ui::layout(&page, &font, 1.0, 200.0);
    let expect = (short.card.bottom - short.scroll.bottom).max(0.0);
    push(
        &mut checks,
        "设置视口矮时可滚动",
        (short.max_scroll() - expect).abs() < 0.5 && expect > 0.0,
        format!("max_scroll={:.1} 期望 {expect:.1}", short.max_scroll()),
    );
    // 按钮与开关并排:开关贴行右,按钮在它左边,两者各自命中。
    let effect_toggle = layout
        .card_hit(ui::Id::new("effects", 0))
        .expect("效果开关矩形");
    let effect_button = layout
        .card_hit(ui::Id::new("open-effects", 0))
        .expect("效果按钮矩形");
    let hit = layout.hit_card(center_x(effect_toggle), center_y(effect_toggle));
    let hit_button = layout.hit_card(center_x(effect_button), center_y(effect_button));
    push(
        &mut checks,
        "设置按钮开关并排",
        hit == Some(ui::Id::new("effects", 0))
            && hit_button == Some(ui::Id::new("open-effects", 0))
            && (effect_button.right <= effect_toggle.left)
            && (effect_toggle.right - toggle.right).abs() < 0.5,
        format!("开关={hit:?} 按钮={hit_button:?}"),
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
