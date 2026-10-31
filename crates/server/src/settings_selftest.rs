//! 设置窗自检:建窗、画一帧、贴一次,再读回像素确认画面不是空的。
//!
//! Windows CI 跑这条,证明原生设置窗这条链路没断:窗口、光栅、位图、
//! GDI 贴图,每一步都得通。报告写到文件里,CI 收进产物。

use super::*;
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};

pub fn selftest(report: &std::path::Path) -> Result<(), String> {
    let mut lines: Vec<String> = Vec::new();
    let result = selftest_run(&mut lines);
    match &result {
        Ok(()) => lines.push("结果:通过".to_string()),
        Err(e) => lines.push(format!("结果:失败 — {e}")),
    }
    let _ = std::fs::write(report, lines.join("\n"));
    result
}

/// 自检的正文。
fn selftest_run(lines: &mut Vec<String>) -> Result<(), String> {
    let hwnd = create_window()?;
    let settings = Settings::default();
    let mut window = Window::new(settings)?;
    window.scale = dpi_of(hwnd);
    window.size = client_size(hwnd);
    window.relayout();
    let (w, h) = window.size;
    lines.push(format!("客户区 {w}×{h} 像素"));
    if w <= 0 || h <= 0 {
        return Err("客户区没有尺寸".to_string());
    }
    // 画一帧。
    {
        let surface = window
            .layer
            .surface(w as u32, h as u32)
            .map_err(|e| format!("光栅表面: {e}"))?;
        let canvas = surface.canvas();
        ui::Painter::new(canvas, &window.palette, &window.font, window.scale).paint(
            &window.page,
            &window.layout,
            None,
            0.0,
        );
    }
    // 像素搬进位图,再贴一次:走通 GDI 那一步。
    if let Err(e) = window.layer.copy_pixels() {
        return Err(format!("搬像素: {e}"));
    }
    unsafe {
        let hdc = GetDC(Some(hwnd));
        let blit = window.layer.blit(hdc, w, h);
        let _ = ReleaseDC(Some(hwnd), hdc);
        blit.map_err(|e| format!("BitBlt: {e}"))?;
    }
    // 读回像素:角落是窗口底色,卡片里是卡片色。
    let pixels = window.layer.pixels().ok_or("像素读回失败")?;
    let at = |x: i32, y: i32| -> (u8, u8, u8) {
        let index = ((y * w + x) * 4) as usize;
        (pixels[index + 2], pixels[index + 1], pixels[index])
    };
    let corner = at(2, 2);
    let expect_corner = bgr(window.palette.window_bg);
    if corner != expect_corner {
        return Err(format!("窗口底色不对: {corner:?} 期望 {expect_corner:?}"));
    }
    // 卡片是圆角的,角上那一点落在圆角外,是窗口底色;取上边缘中点
    // 才是卡片底色。
    let card_rect = window.layout.card;
    let card_x = (card_rect.left + card_rect.width() / 2.0) as i32;
    let card_y = card_rect.top as i32 + 2;
    let card = at(card_x, card_y);
    let expect_card = bgr(window.palette.bg);
    if card != expect_card {
        return Err(format!("卡片底色不对: {card:?} 期望 {expect_card:?}"));
    }
    lines.push(format!("窗口底色 {corner:?} 卡片底色 {card:?}"));
    // 命中区能对上:第一个开关点得到。
    let toggle = ui::Id::new(ids::AUX, 0);
    let rect = window
        .layout
        .card_hit(toggle)
        .ok_or("找不到辅助码开关的命中区")?;
    let hit = window.layout.hit_card(
        (rect.left + rect.right) / 2.0,
        (rect.top + rect.bottom) / 2.0,
    );
    if hit != Some(toggle) {
        return Err(format!("开关命中不对: {hit:?}"));
    }
    lines.push("开关命中正常".to_string());
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}
