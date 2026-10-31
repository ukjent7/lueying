//! SkSL 效果层 E2E:模板编译、uniform 打包、坏效果隔离与帧预算。

use std::time::{Duration, Instant};

use lueying_render::effect::{self, Uniforms};
use lueying_render::panel::{self, Metrics};
use skia_safe::{Paint, surfaces};

use crate::harness::{palette, push, read_bgr, rgb, spec};

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

pub(crate) fn effect_checks(checks: &mut Vec<(&'static str, bool, String)>) {
    let metrics = Metrics::standard();

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
                checks,
                "SkSL 背景效果",
                shader_ok && (200..=255).contains(&blue),
                format!("shader_ok={shader_ok} B={blue}"),
            );
        }
        Err(e) => push(checks, "SkSL 背景效果", false, e),
    }
    let bad = effect::compile_background("half4 main(float2 p) { return nonexistent; }");
    push(
        checks,
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
                        checks,
                        "效果槽位出图",
                        // 效果确实画上去了(与素色不同),又被遮罩拉回可读区间。
                        pixel != plain && pixel.0 >= 128 && body >= 4.5 && weak >= 3.0,
                        format!("{pixel:?} 素色 {plain:?} 正文 {body:.2} 弱文字 {weak:.2}"),
                    );
                }
                None => push(checks, "效果槽位出图", false, "像素读回失败".into()),
            }
        }
        Err(e) => push(checks, "效果槽位出图", false, e),
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
        checks,
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
        push(checks, "坏效果只废一个槽位", false, "临时目录写不了".into());
    } else {
        let set = effect::EffectSet::load_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        push(
            checks,
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
            checks,
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
        checks,
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
        checks,
        "帧预算摘效果",
        blocked && allowed,
        format!("冷却内 blocked={blocked} 冷却后 allowed={allowed}"),
    );
}
