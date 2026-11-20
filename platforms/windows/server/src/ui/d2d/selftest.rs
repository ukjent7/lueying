//! D2D 候选窗自检:离屏出帧、读回像素断言。
//!
//! WARP(软件 D3D11)渲染在服务端进程里跑,给出确定性的像素断言,外加
//! 一条字体断言——DirectWrite 取到的字型必须真的画进画面。没有桌面会话
//! 的 CI 照样能跑;窗口与 DComp 的真实上屏由 Windows E2E 在真机覆盖。

use lueying_render::panel::{self, GridItem, Item, Layout, Metrics, Palette, PanelSpec};
use lueying_server::theme;

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, ID2D1Image};
use windows::core::Interface;

use crate::ui::d2d::glass;
use crate::ui::d2d::pipe::{Frame, Morph, Target, Text, rgb};

/// 玻璃预览:`lueying-server.exe --glass-preview <路径前缀>`。
/// 合成桌面(色块 + 文档行)走与实机同一条捕获效果链,亮暗各叠一帧
/// 玻璃存 BMP 供人眼核对;独立进程,不碰单实例锁与管道。
pub(crate) fn glass_preview(prefix: &std::path::Path) {
    let Ok(text) = Text::new() else {
        println!("玻璃预览失败:文字引擎起不来");
        return;
    };
    let metrics = Metrics::standard();
    let spec = preview_spec();
    let width = panel::panel_width(&spec, &metrics, &text) as u32;
    let height = panel::panel_height(&spec, &metrics, &text) as u32;
    let m = glass::margin(1.0);
    // 场景用整屏尺寸复现实机捕获规模(小场景复不出大输入下的问题)。
    let cw = 2560;
    let ch = 1440;

    for (name, dark) in [("bright", false), ("dark", true)] {
        let mut scene = match crate::ui::d2d::pipe::Offscreen::new(cw as u32, ch as u32) {
            Ok(target) => target,
            Err(e) => {
                println!("玻璃预览失败:建场景管线 {e}");
                return;
            }
        };
        if let Err(e) = scene.draw_with(|dc| paint_scene(dc, cw, ch, dark, m)) {
            println!("玻璃预览失败:画场景 {e}");
            return;
        }
        let Some(bgra) = scene.read_bgra(cw as u32, ch as u32) else {
            println!("玻璃预览失败:场景读回");
            return;
        };
        let _ = save_bmp(
            &prefix.with_extension(format!("{name}.scene.bmp")),
            &bgra,
            cw as u32,
            ch as u32,
        );
        let palette = super::glass_ink(theme::palette(theme::colors_with(
            theme::Appearance::Dark,
            0,
        )));
        // 一个进程同时持有两个 Offscreen D3D 设备会静默崩:条帧的 target/lens
        // 先在这个块里释放,再建棋盘与形变帧。
        {
            let mut target = match crate::ui::d2d::pipe::Offscreen::new(width, height) {
                Ok(target) => target,
                Err(e) => {
                    println!("玻璃预览失败:建管线 {e}");
                    return;
                }
            };
            let dc = target.context();
            let Some(bitmap) = glass::upload(
                &dc,
                cw as u32,
                ch as u32,
                &bgra,
                96.0,
                windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE_IGNORE,
            ) else {
                println!("玻璃预览失败:背板上传");
                return;
            };
            let _ = target.fill_all(solid(0.0, 0.0, 0.0));
            let _ = target.draw_with(|dc| {
                glass::draw_backdrop(dc, &bitmap.cast::<ID2D1Image>().unwrap(), (0.0, 0.0))
            });
            if let Some(bgra) = target.read_bgra(width, height) {
                let _ = save_bmp(
                    &prefix.with_extension(format!("{name}.sharp.bmp")),
                    &bgra,
                    width,
                    height,
                );
            }
            let Some(lens) = glass::lens(&dc, width, height, 1.0, panel::PANEL_RADIUS) else {
                println!("玻璃预览失败:透镜");
                return;
            };
            let Ok(sharp) = bitmap.cast::<ID2D1Image>() else {
                println!("玻璃预览失败:背板转图像");
                return;
            };
            let Some(images) = lens.image(&dc, &sharp, (m as f32, m as f32), m as f32) else {
                println!("玻璃预览失败:折射");
                return;
            };
            // 分层 dump:身体/透镜/alpha 环各存一张,调参时定位哪层在漏。
            // 必须在同一 target 设备上画:效果输出图像跨 D3D 设备画会静默失败。
            let off = (-m as f32, -m as f32);
            for (tag, img) in [
                ("body", &images.body),
                ("lens", &images.lens),
                ("ring", &images.ring.cast::<ID2D1Image>().unwrap()),
            ] {
                let _ = target.fill_all(solid(0.0, 0.0, 0.0));
                let _ = target.draw_with(|dc| glass::draw_backdrop(dc, img, off));
                if let Some(bgra) = target.read_bgra(width, height) {
                    let _ = save_bmp(
                        &prefix.with_extension(format!("{name}.{tag}.bmp")),
                        &bgra,
                        width,
                        height,
                    );
                }
            }
            for (tag, bmp) in lens.debug_stages(&dc, m as f32) {
                let _ = target.fill_all(solid(0.0, 0.0, 0.0));
                let Ok(img) = bmp.cast::<ID2D1Image>() else {
                    continue;
                };
                let _ = target.draw_with(|dc| glass::draw_backdrop(dc, &img, off));
                if let Some(bgra) = target.read_bgra(width, height) {
                    let _ = save_bmp(
                        &prefix.with_extension(format!("{name}.{tag}.bmp")),
                        &bgra,
                        width,
                        height,
                    );
                }
            }
            if let Err(e) = target.fill_all(solid(0.0, 0.0, 0.0)) {
                println!("玻璃预览失败:铺底 {e}");
                return;
            }
            let frame = Frame {
                spec: &spec,
                palette: &palette,
                metrics,
                glass: true,
                backdrop: Some((&images, (-m as f32, -m as f32))),
                opacity: 1.0,
                list: panel::ListFx::NEUTRAL,
                hover: None,
                chev: false,
                scroll_hover: false,
                morph: None,
                menu: None,
                menu_hover: None,
            };
            if let Err(e) = target.paint_over(&frame, &text) {
                println!("玻璃预览失败:出帧 {e}");
                return;
            }
            let Some(bgra) = target.read_bgra(width, height) else {
                println!("玻璃预览失败:纹理读回");
                return;
            };
            let path = prefix.with_extension(format!("{name}.bmp"));
            match save_bmp(&path, &bgra, width, height) {
                Ok(()) => println!("玻璃预览已写出 {}", path.display()),
                Err(e) => println!("玻璃预览失败:写 {e}"),
            }
        }

        // 展开棋盘:同一条玻璃链铺 480×210,检四列等宽格子与首行收起钮。
        let grid = demo_expanded();
        let gh = panel::panel_height(&grid, &metrics, &text) as u32;
        if let Err(e) = preview_glass(
            prefix,
            &format!("{name}.grid"),
            &grid,
            None,
            Some(gh),
            &bgra,
            cw,
            ch,
            &text,
            metrics,
            &palette,
            m,
        ) {
            println!("玻璃预览失败: {e}");
            return;
        }
        // 形变中间帧:玻璃长到 45%,固定条内容退场、棋盘内容进场。
        let strip = preview_spec();
        let from_h = panel::panel_height(&strip, &metrics, &text);
        let to_h = panel::panel_height(&grid, &metrics, &text);
        let progress = 0.45;
        let height = (from_h + (to_h - from_h) * progress).round() as u32;
        let morph = Morph {
            from: &strip,
            progress,
            height: height as f32,
            up: false,
            expanding: true,
        };
        if let Err(e) = preview_glass(
            prefix,
            &format!("{name}.morph"),
            &grid,
            Some(morph),
            Some(height),
            &bgra,
            cw,
            ch,
            &text,
            metrics,
            &palette,
            m,
        ) {
            println!("玻璃预览失败: {e}");
            return;
        }
    }
}

/// 在同一张合成背景上铺一帧玻璃;`height` 给形变中间帧用窗口高度,
/// 静止帧取面板高度(含首行角上收起钮的棋盘也走这里)。
#[allow(clippy::too_many_arguments)]
fn preview_glass(
    prefix: &std::path::Path,
    tag: &str,
    spec: &PanelSpec,
    morph: Option<Morph<'_>>,
    height: Option<u32>,
    scene: &[u8],
    cw: i32,
    ch: i32,
    text: &Text,
    metrics: Metrics,
    palette: &Palette,
    m: i32,
) -> Result<(), String> {
    let width = panel::panel_width(spec, &metrics, text) as u32;
    let height = height.unwrap_or_else(|| panel::panel_height(spec, &metrics, text) as u32);
    let mut target =
        crate::ui::d2d::pipe::Offscreen::new(width, height).map_err(|e| format!("建管线 {e}"))?;
    let dc = target.context();
    let bitmap = glass::upload(
        &dc,
        cw as u32,
        ch as u32,
        scene,
        96.0,
        windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE_IGNORE,
    )
    .ok_or_else(|| "背板上传".to_string())?;
    target
        .fill_all(solid(0.0, 0.0, 0.0))
        .map_err(|e| format!("铺底 {e}"))?;
    target
        .draw_with(|dc| glass::draw_backdrop(dc, &bitmap.cast::<ID2D1Image>().unwrap(), (0.0, 0.0)))
        .map_err(|e| format!("清晰层 {e}"))?;
    let lens = glass::lens(&dc, width, height, 1.0, panel::PANEL_RADIUS)
        .ok_or_else(|| "透镜".to_string())?;
    let images = lens
        .image(
            &dc,
            &bitmap.cast::<ID2D1Image>().unwrap(),
            (m as f32, m as f32),
            m as f32,
        )
        .ok_or_else(|| "折射".to_string())?;
    target
        .fill_all(solid(0.0, 0.0, 0.0))
        .map_err(|e| format!("铺底 {e}"))?;
    let frame = Frame {
        spec,
        palette,
        metrics,
        glass: true,
        backdrop: Some((&images, (-m as f32, -m as f32))),
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: None,
        chev: false,
        scroll_hover: false,
        morph,
        menu: None,
        menu_hover: None,
    };
    target
        .paint_over(&frame, text)
        .map_err(|e| format!("出帧 {e}"))?;
    let bgra = target
        .read_bgra(width, height)
        .ok_or_else(|| "纹理读回".to_string())?;
    let path = prefix.with_extension(format!("{tag}.bmp"));
    save_bmp(&path, &bgra, width, height)?;
    println!("玻璃预览已写出 {}", path.display());
    Ok(())
}

/// 合成桌面:底色 + 高饱和色块 + 文档窗与文本行,模糊采样有的看;
/// 两条高对比直线穿过玻璃区,折射在边缘把它们弯折,肉眼可验。
fn paint_scene(dc: &ID2D1DeviceContext, w: i32, h: i32, dark: bool, m: i32) {
    unsafe {
        let brush = dc
            .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
            .expect("场景画刷");
        let (base, doc, line) = if dark {
            (
                solid(0.10, 0.12, 0.18),
                solid(0.16, 0.18, 0.24),
                solid(0.35, 0.38, 0.45),
            )
        } else {
            (
                solid(0.88, 0.86, 0.82),
                solid(0.97, 0.96, 0.94),
                solid(0.55, 0.55, 0.58),
            )
        };
        let rect = |x: i32, y: i32, rw: i32, rh: i32| D2D_RECT_F {
            left: x as f32,
            top: y as f32,
            right: (x + rw) as f32,
            bottom: (y + rh) as f32,
        };
        brush.SetColor(&base);
        dc.FillRectangle(&rect(0, 0, w, h), &brush);
        for (i, c) in [
            solid(0.91, 0.58, 0.16),
            solid(0.20, 0.45, 0.85),
            solid(0.30, 0.65, 0.35),
        ]
        .iter()
        .enumerate()
        {
            brush.SetColor(c);
            let x = (w as f32 * (0.08 + 0.3 * i as f32)) as i32;
            dc.FillRectangle(&rect(x, h / 6, w / 5, h / 3), &brush);
        }
        brush.SetColor(&doc);
        dc.FillRectangle(&rect(w / 8, h / 2, w * 3 / 4, h * 2 / 5), &brush);
        brush.SetColor(&line);
        for i in 0..4 {
            let y = h / 2 + 6 + i * 8;
            dc.FillRectangle(&rect(w / 8 + 8, y, w * 3 / 5 - (i % 2) * 40, 3), &brush);
        }
        brush.SetColor(&solid(0.95, 0.95, 0.95));
        dc.FillRectangle(&rect(m + 24, 0, 3, h), &brush);
        dc.FillRectangle(&rect(0, m + 10, w, 3), &brush);
        // 细字代理:3px 短划排成两行;身体模糊真生效时应当完全糊掉。
        brush.SetColor(&line);
        for row in 0..2 {
            for k in 0..24 {
                let x = m + 40 + k * 14;
                let y = m + 16 + row * 10;
                dc.FillRectangle(&rect(x, y, 6, 3), &brush);
            }
        }
    }
}

fn solid(r: f32, g: f32, b: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a: 1.0 }
}

/// BGRA 纹理写 24 位 BMP;行距按 4 字节对齐。
pub(super) fn save_bmp(
    path: &std::path::Path,
    bgra: &[u8],
    width: u32,
    height: u32,
) -> Result<(), String> {
    let row = (width as usize * 3).div_ceil(4) * 4;
    let data_size = row * height as usize;
    let mut file = Vec::with_capacity(54 + data_size);
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&((54 + data_size) as u32).to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&54u32.to_le_bytes());
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&(width as i32).to_le_bytes());
    file.extend_from_slice(&(height as i32).to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&24u16.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&(data_size as u32).to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes());
    let pad = row - width as usize * 3;
    // BMP 自下而上;BGRA 摘 BGR。
    for y in (0..height as usize).rev() {
        for x in 0..width as usize {
            let at = (y * width as usize + x) * 4;
            file.extend_from_slice(&[bgra[at], bgra[at + 1], bgra[at + 2]]);
        }
        file.extend(std::iter::repeat(0u8).take(pad));
    }
    std::fs::write(path, file).map_err(|e| format!("{}: {e}", path.display()))
}

/// 预览用候选:长短混合,贴近真实条面。
fn preview_spec() -> PanelSpec {
    let mut spec = demo_spec();
    spec.items = vec![
        Item {
            text: "实施".into(),
            hint: String::new(),
            selected: true,
        },
        Item {
            text: "实时".into(),
            hint: String::new(),
            selected: false,
        },
        Item {
            text: "试试".into(),
            hint: String::new(),
            selected: false,
        },
        Item {
            text: "是是".into(),
            hint: String::new(),
            selected: false,
        },
        Item {
            text: "事实".into(),
            hint: String::new(),
            selected: false,
        },
    ];
    spec
}

/// CI 自检:离屏 WARP 出帧 + 像素断言,结论写日志。
pub(crate) fn selftest() {
    let Ok(text) = Text::new() else {
        println!("D2D 自检失败:文字引擎起不来");
        return;
    };
    let metrics = Metrics::standard();
    let palette = theme::palette(theme::colors_with(theme::Appearance::Light, 0));

    // 固定条:恒定尺寸、选中槽底色、字体真的画得出来。
    let strip = demo_spec();
    let width = panel::panel_width(&strip, &metrics, &text) as u32;
    let height = panel::panel_height(&strip, &metrics, &text) as u32;
    let mut target = match crate::ui::d2d::pipe::Offscreen::new(width, height) {
        Ok(target) => target,
        Err(e) => {
            println!("D2D 自检失败:离屏渲染建管线 {e}");
            return;
        }
    };
    match check_strip(&mut target, &strip, &palette, &metrics, &text) {
        Ok(()) => println!("D2D 自检通过:固定条帧读回一致({width}x{height})"),
        Err(e) => println!("D2D 自检失败:固定条像素 {e}"),
    }
    match font_check(&mut target, &strip, &palette, &metrics, &text) {
        Ok(()) => println!("D2D 自检通过:DirectWrite 字型已画出"),
        Err(FontCheck::NoFont) => println!("D2D 字体自检:系统没有可用字型,跳过"),
        Err(FontCheck::Failed(e)) => println!("D2D 自检失败:字体 {e}"),
    }

    // 展开网格:两段布局、网格文字与菜单底色的叠加顺序。
    let expanded = demo_expanded();
    let width = panel::panel_width(&expanded, &metrics, &text) as u32;
    let height = panel::panel_height(&expanded, &metrics, &text) as u32;
    let mut target = match crate::ui::d2d::pipe::Offscreen::new(width, height) {
        Ok(target) => target,
        Err(e) => {
            println!("D2D 自检失败:展开态建管线 {e}");
            return;
        }
    };
    match check_expanded(&mut target, &expanded, &palette, &metrics, &text) {
        Ok(()) => println!("D2D 自检通过:展开网格帧读回一致({width}x{height})"),
        Err(e) => println!("D2D 自检失败:展开网格像素 {e}"),
    }
}

/// 底色之上的悬停底色;`at` 是悬停块内的采样点。
fn hover_pixel(
    target: &mut dyn Target,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    text: &Text,
    index: usize,
    at: (i32, i32),
) -> Result<(), String> {
    target
        .paint_frame(
            &Frame {
                spec,
                palette,
                metrics: *metrics,
                glass: false,

                backdrop: None,
                opacity: 1.0,
                list: panel::ListFx::NEUTRAL,
                hover: Some(index),
                chev: false,
                scroll_hover: false,
                morph: None,
                menu: None,
                menu_hover: None,
            },
            text,
        )
        .map_err(|e| format!("出帧失败: {e}"))?;
    let expected = rgb(palette.hover);
    let pixel = target.pixel(at.0, at.1).ok_or("像素读回失败(悬停)")?;
    if pixel != expected {
        return Err(format!("悬停像素 {pixel:?} 期望 {expected:?}"));
    }
    Ok(())
}

/// 固定条帧:底色与选中槽的底色。
fn check_strip(
    target: &mut dyn Target,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    text: &Text,
) -> Result<(), String> {
    let frame = Frame {
        spec,
        palette,
        metrics: *metrics,
        glass: false,

        backdrop: None,
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: None,
        chev: false,
        scroll_hover: false,
        morph: None,
        menu: None,
        menu_hover: None,
    };
    target
        .paint_frame(&frame, text)
        .map_err(|e| format!("出帧失败: {e}"))?;
    let expected_bg = rgb(palette.bg);
    // 底边距是纯底色:固定条只有一行,槽底下这条带不被任何内容碰。
    // x 取在圆角之外的中段,别让采样点落进窗口圆角的透明区。
    let bg = target
        .pixel(20, height_of(spec, metrics, text) as i32 - 3)
        .ok_or("像素读回失败(背景)")?;
    if bg != expected_bg {
        return Err(format!("背景像素 {bg:?} 期望 {expected_bg:?}"));
    }
    // 选中槽是上亮下深的胶囊:右侧留白处取上下两点,亮面必须上亮下深,
    // 且都离开面板底色,证明渐变真的落在槽里。
    let rects = panel::item_rects(spec, metrics, text);
    let slot = rects.first().ok_or("固定条至少要一项")?;
    let x = slot.right as i32 - 6;
    let y = slot.center_y() as i32;
    let hi = target.pixel(x, y - 6).ok_or("像素读回失败(选中槽上缘)")?;
    let lo = target.pixel(x, y + 6).ok_or("像素读回失败(选中槽下缘)")?;
    let sum = |p: (u8, u8, u8)| u32::from(p.0) + u32::from(p.1) + u32::from(p.2);
    if sum(hi) <= sum(lo) || hi == rgb(palette.bg) || lo == rgb(palette.bg) {
        return Err(format!(
            "选中槽上下缘 {hi:?}/{lo:?} 未形成亮面渐变(背景 {:?})",
            rgb(palette.bg)
        ));
    }
    Ok(())
}

/// 展开网格帧:网格文字画得出来,悬停块压在第二页项上。
fn check_expanded(
    target: &mut dyn Target,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    text: &Text,
) -> Result<(), String> {
    let height = height_of(spec, metrics, text);
    let grid = panel::grid_rects(spec, metrics, text);
    let cell = grid.first().ok_or("展开态至少要一格网格")?;
    // 网格文字:单元中央必须有与底色不同的像素(文字真的画了)。
    target
        .paint_frame(
            &Frame {
                spec,
                palette,
                metrics: *metrics,
                glass: false,

                backdrop: None,
                opacity: 1.0,
                list: panel::ListFx::NEUTRAL,
                hover: None,
                chev: false,
                scroll_hover: false,
                morph: None,
                menu: None,
                menu_hover: None,
            },
            text,
        )
        .map_err(|e| format!("出帧失败: {e}"))?;
    let expected_bg = rgb(palette.bg);
    let mut drew_text = false;
    'scan: for x in (cell.left as i32 + 2)..(cell.right as i32 - 2) {
        for y in (cell.top as i32 + 2)..(cell.bottom as i32 - 2) {
            if target.pixel(x, y) != Some(expected_bg) {
                drew_text = true;
                break 'scan;
            }
        }
    }
    if !drew_text {
        return Err("网格单元全是底色,文字没画出来".into());
    }
    // 悬停:第二页项右缘留白处仍是悬停底色。
    let rects = panel::item_rects(spec, metrics, text);
    let slot = rects.get(1).ok_or("展开态至少要两项")?;
    hover_pixel(
        target,
        spec,
        palette,
        metrics,
        text,
        1,
        (slot.right as i32 - 3, slot.center_y() as i32),
    )?;
    // 菜单底色压在候选区上:画序在最后,压住先画的一切。
    let menu_y = height as f32 - panel::menu_height(metrics);
    target
        .paint_frame(
            &Frame {
                spec,
                palette,
                metrics: *metrics,
                glass: false,

                backdrop: None,
                opacity: 1.0,
                list: panel::ListFx::NEUTRAL,
                hover: None,
                chev: false,
                scroll_hover: false,
                morph: None,
                menu: Some((8.0, menu_y)),
                menu_hover: None,
            },
            text,
        )
        .map_err(|e| format!("菜单帧出帧失败: {e}"))?;
    let covered = target
        .pixel(30, (menu_y + 8.0) as i32)
        .ok_or("像素读回失败(菜单)")?;
    if covered != expected_bg {
        return Err(format!("菜单像素 {covered:?} 期望 {expected_bg:?}"));
    }
    Ok(())
}

fn height_of(spec: &PanelSpec, metrics: &Metrics, text: &Text) -> u32 {
    panel::panel_height(spec, metrics, text) as u32
}

enum FontCheck {
    NoFont,
    Failed(String),
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
        glass: false,

        backdrop: None,
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: None,
        chev: false,
        scroll_hover: false,
        morph: None,
        menu: None,
        menu_hover: None,
    };
    target
        .paint_frame(&frame, text)
        .map_err(FontCheck::Failed)?;
    let rects = panel::item_rects(spec, metrics, text);
    let row = rects.first().ok_or(FontCheck::NoFont)?;
    // 渐变逐行不同,参考色按同一 y 在右侧留白取;扫中线附近找文字墨迹。
    let (cx, cy) = (row.right as i32 - 6, row.center_y() as i32);
    for y in (cy - 7)..=(cy + 7) {
        let Some(fill) = target.pixel(cx, y) else {
            continue;
        };
        for x in (row.left as i32 + 2)..(row.right as i32 - 10) {
            if let Some(pixel) = target.pixel(x, y)
                && pixel != fill
            {
                return Ok(());
            }
        }
    }
    Err(FontCheck::Failed("行内全是底色,文字没画出来".into()))
}

/// 自检用固定条;内容与 Linux E2E 的 `render::spec()` 一致。
pub(super) fn demo_spec() -> PanelSpec {
    PanelSpec {
        width: 360.0,
        max_text_width: 490.0,
        expand: false,
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
        grid: Vec::new(),
        scroll: None,
        layout: Layout::Wrap,
    }
}

/// 自检用展开态:24 条候选铺满 6 行棋盘,收起钮浮在首行右端。
fn demo_expanded() -> PanelSpec {
    let mut spec = demo_spec();
    spec.expand = true;
    spec.grid = (0..panel::GRID_MAX - spec.items.len())
        .map(|i| GridItem {
            text: format!("候选词{i}"),
            index: 2 + i,
            selected: false,
        })
        .collect();
    spec
}
