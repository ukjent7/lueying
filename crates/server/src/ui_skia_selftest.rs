//! Skia 候选窗自检:出帧、读回像素断言。
//!
//! 分两层跑:
//!
//! - 光栅(CPU)路径在服务端进程里跑,给出确定性的像素断言(与 Linux E2E 同一套
//!   期望值),外加一条字体断言——DirectWrite 取到的字型必须真的画进画面。
//! - Ganesh-D3D12 路径另起子进程跑。没有硬件适配器(虚拟机、CI runner)或驱动
//!   异常的机器上,GPU 管线会把进程带崩(访问冲突);子进程崩了服务端照常活着,
//!   日志如实记下原因,不把输入法拖下水。

use std::time::{Duration, Instant};

use lueying_render::panel::{self, Item, Layout, Metrics, Palette, PanelSpec, PreeditBar};
use lueying_server::theme;
use skia_safe::Font;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, HWND_TOPMOST, IsWindow, SWP_NOACTIVATE, SetWindowPos,
};

use crate::ui_skia::{Presenter, cpu_presenter, create_window, panel_font, swap_window};
use crate::ui_skia_gpu::{Frame, FrameTarget, Offscreen, Pipeline, Raster, rgb};

/// GPU 自检子进程的上限;超时就杀掉,GPU 管线卡死不该拖住输入法启动。
const GPU_CHILD_TIMEOUT: Duration = Duration::from_secs(20);

/// CI 自检:光栅断言(本进程)+ GPU 断言(子进程),结论写日志。
pub(super) fn selftest() {
    let metrics = Metrics::standard();
    let spec = demo_spec();
    let width = panel::panel_width(&spec, &metrics, None);
    let height = panel::panel_height(&spec, &metrics, None);
    let palette = theme::palette(theme::colors_with(theme::Appearance::Light, 0));
    // 光栅:确定性的像素期望,任何环境都能跑。
    match Raster::new(width as u32, height as u32) {
        Ok(mut target) => match check_pixels(&mut target, &spec, &palette, &metrics) {
            Ok(()) => println!(
                "SKIA 自检通过:光栅帧读回一致({}x{})",
                width as u32, height as u32
            ),
            Err(e) => println!("SKIA 自检失败:光栅渲染 {e}"),
        },
        Err(e) => println!("SKIA 自检失败:光栅渲染建面 {e}"),
    }
    // 字体:字型必须真的画进画面。系统一个可用字型都没有是环境问题,记下来
    // 不算失败;字型拿到了却画不出东西才是真 bug。
    match font_check(&spec, &palette, &metrics, width as u32, height as u32) {
        Ok(()) => println!("SKIA 自检通过:DirectWrite 字型已画出"),
        Err(FontCheck::NoFont) => println!("SKIA 字体自检:系统没有可用字型,跳过"),
        Err(FontCheck::Failed(e)) => println!("SKIA 自检失败:字体 {e}"),
    }
    // CPU 兜底:没有硬件 D3D12 的机器上候选窗靠这条路,上屏与像素都要过。
    // 这条失败不拖垮整轮 E2E(分层窗口在某些无桌面会话里本来就建不出来),
    // 结论写进报告由人看。
    match cpu_check(&spec, &palette, &metrics, width as u32, height as u32) {
        Ok(()) => println!("SKIA CPU 自检通过:CPU 光栅上屏一致"),
        Err(e) => println!("SKIA CPU 自检失败:{e}"),
    }
    // 换手:设备被摘掉后要能换到 CPU 光栅接着弹,候选窗不整块消失。
    match swap_check(&spec, &palette, &metrics, width as u32, height as u32) {
        Ok(()) => println!("SKIA 换手自检通过:换窗口后画面与上屏一致"),
        Err(e) => println!("SKIA 换手自检失败:{e}"),
    }
    // GPU:另起子进程。崩了只影响子进程。
    match gpu_child("LUEYING_SKIA_SELFTEST_GPU") {
        Ok(text) => println!("{text}"),
        Err(e) => println!("SKIA GPU 自检不可用:{e}"),
    }
}

/// GPU 自检(子进程):合成交换链 → 离屏 D3D12,两条都建不起来就算不可用。
pub(super) fn gpu_selftest() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let metrics = Metrics::standard();
    let spec = demo_spec();
    let width = panel::panel_width(&spec, &metrics, None);
    let height = panel::panel_height(&spec, &metrics, None);
    let palette = theme::palette(theme::colors_with(theme::Appearance::Light, 0));
    let (mode, outcome) = match window_target(width as u32, height as u32) {
        Ok((pipe, hwnd)) => {
            let mut presenter = Presenter::Gpu(pipe);
            let outcome = check_pixels(&mut presenter, &spec, &palette, &metrics);
            // 设备被摘掉时的换手:GPU 窗口换成 CPU 分层窗口,画面与上屏要照旧。
            // 这条真跑 GPU → CPU,崩了也只影响这个子进程。
            let swapped = handover(
                hwnd,
                &mut presenter,
                &spec,
                &palette,
                &metrics,
                width as u32,
                height as u32,
            );
            match swapped {
                Ok(()) => println!("SKIA GPU 换手自检通过:GPU 窗口换成 CPU 光栅后画面一致"),
                Err(e) => println!("SKIA GPU 换手自检失败:{e}"),
            }
            // 先放管线再销窗:DComp 目标还指着这个 HWND(换手成功时窗口已经换过)。
            drop(presenter);
            if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            ("合成交换链", outcome)
        }
        Err(e) => {
            // 桌面会话缺失或合成交换链建不起来时,退回离屏渲染验证出帧。
            println!("SKIA GPU 自检降级:合成交换链不可用({e}),改走离屏渲染");
            match Offscreen::new(width as u32, height as u32) {
                Ok(mut target) => (
                    "离屏渲染",
                    check_pixels(&mut target, &spec, &palette, &metrics),
                ),
                Err(e) => {
                    println!("SKIA GPU 自检不可用:离屏渲染建管线 {e}");
                    return;
                }
            }
        }
    };
    match outcome {
        Ok(()) => println!(
            "SKIA GPU 自检通过:{mode}帧读回一致({}x{})",
            width as u32, height as u32
        ),
        Err(e) => println!("SKIA GPU 自检失败:{mode} {e}"),
    }
}

/// 起子进程跑 GPU 侧任务。子进程崩了只影响它自己,服务端继续活着;
/// 自检与 V3 基准共用,`env_name` 区分任务。
pub(super) fn gpu_child(env_name: &str) -> Result<String, String> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return Err(format!("取可执行文件路径 {e}")),
    };
    let mut child = std::process::Command::new(exe)
        .env(env_name, "1")
        .stdout(std::process::Stdio::piped())
        // 子进程的 stderr 直接进服务端日志:Skia 的报错都在那儿。
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("起子进程 {e}"))?;
    // 管道得有人读,否则子进程写满缓冲就卡住。
    let reader = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut pipe, &mut buf);
            buf
        })
    });
    let deadline = Instant::now() + GPU_CHILD_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("等子进程 {e}")),
        }
    };
    let text = match reader {
        Some(handle) => handle.join().unwrap_or_default(),
        None => String::new(),
    };
    let Some(status) = status else {
        return Err(format!("子进程超时({}秒)", GPU_CHILD_TIMEOUT.as_secs()));
    };
    let text = text.trim().to_string();
    if !status.success() {
        let code = status.code().map_or(0, |c| c as u32);
        return Err(format!("子进程退出码 0x{code:08X}"));
    }
    if text.is_empty() {
        return Err("子进程没有输出".into());
    }
    Ok(text)
}

/// 字体自检的两种结果:系统没字型是环境问题,画不出东西才是 bug。
enum FontCheck {
    NoFont,
    Failed(String),
}

/// 带字型再画一帧,像素必须和无字型那帧不同;相同就说明字型没进画面。
fn font_check(
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    w: u32,
    h: u32,
) -> Result<(), FontCheck> {
    let Some(font) = panel_font() else {
        return Err(FontCheck::NoFont);
    };
    let plain = raster_pixels(spec, palette, metrics, w, h, None)?;
    let with = raster_pixels(spec, palette, metrics, w, h, Some(&font))?;
    if plain == with {
        return Err(FontCheck::Failed(
            "带字型那帧与无字型那帧像素相同,字型没画进画面".into(),
        ));
    }
    Ok(())
}

/// 光栅画一帧,读回整幅像素。
fn raster_pixels(
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    w: u32,
    h: u32,
    font: Option<&Font>,
) -> Result<Vec<u8>, FontCheck> {
    let mut target = Raster::new(w, h).map_err(FontCheck::Failed)?;
    let f = frame(spec, palette, metrics, font);
    target.draw(f).map_err(FontCheck::Failed)?;
    match target.pixels() {
        Some(buf) => Ok(buf),
        None => Err(FontCheck::Failed("像素读回失败".into())),
    }
}

/// CPU 兜底路径:建分层窗口、画一帧、贴上屏,像素断言与 GPU 路径同一套。
/// 没有硬件 D3D12 的机器上候选窗走这条路,上屏这步不通就等于没有候选窗。
fn cpu_check(
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    w: u32,
    h: u32,
) -> Result<(), String> {
    let (hwnd, mut pipe) = cpu_presenter()?;
    if let Err(e) = pipe.rebuild_surfaces(w, h) {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return Err(e);
    }
    let outcome = check_pixels(&mut pipe, spec, palette, metrics);
    let presented = pipe.present();
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    outcome?;
    presented?;
    Ok(())
}

/// 换手自检:旧窗口换成 CPU 分层窗口,新窗口要接手、旧窗口要销毁、画面与上屏照旧。
///
/// 服务端进程里从 CPU 那条换起(不碰 GPU,崩不了);GPU → CPU 的换手在 GPU
/// 子进程里跑,崩了也只影响子进程。
fn swap_check(
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    w: u32,
    h: u32,
) -> Result<(), String> {
    let (hwnd, cpu) = cpu_presenter()?;
    let mut pipe = Presenter::Cpu(cpu);
    let outcome = handover(hwnd, &mut pipe, spec, palette, metrics, w, h);
    // 换手成功时旧窗口已经销毁,这里再销一次是空操作。
    if unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
    outcome
}

/// 把旧窗口换成 CPU 分层窗口,再走一遍像素与上屏断言。
fn handover(
    old_hwnd: HWND,
    pipe: &mut Presenter,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    w: u32,
    h: u32,
) -> Result<(), String> {
    let new_hwnd = swap_window(old_hwnd, pipe)?;
    // 新窗口要接手,旧窗口要销毁。
    let new_alive = unsafe { IsWindow(Some(new_hwnd)) }.as_bool();
    let old_alive = unsafe { IsWindow(Some(old_hwnd)) }.as_bool();
    let result = if !new_alive {
        Err("新窗口没有建起来".into())
    } else if old_alive {
        Err("旧窗口没有销毁".into())
    } else {
        repaint(new_hwnd, pipe, spec, palette, metrics, w, h)
    };
    unsafe {
        let _ = DestroyWindow(new_hwnd);
    }
    result
}

/// 换手后的新窗口:摆到指定位置、重建表面,像素与上屏都要照旧。
fn repaint(
    new_hwnd: HWND,
    pipe: &mut Presenter,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    w: u32,
    h: u32,
) -> Result<(), String> {
    unsafe {
        let _ = SetWindowPos(
            new_hwnd,
            Some(HWND_TOPMOST),
            120,
            160,
            w as i32,
            h as i32,
            SWP_NOACTIVATE,
        );
    }
    pipe.rebuild_surfaces(w, h)?;
    check_pixels(&mut *pipe, spec, palette, metrics)?;
    pipe.present()
}

/// 建窗 + DComp 交换链;失败时把原因带回去,自检与 V3 基准据此降级。
pub(super) fn window_target(w: u32, h: u32) -> Result<(Pipeline, HWND), String> {
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

/// 一帧的绘制输入;各条出帧路径共用。
fn frame<'a>(
    spec: &'a PanelSpec,
    palette: &'a Palette,
    metrics: &'a Metrics,
    font: Option<&'a Font>,
) -> Frame<'a> {
    Frame {
        spec,
        palette,
        metrics,
        font,
        opacity: 1.0,
        list: panel::ListFx::NEUTRAL,
        hover: None,
        pager: None,
        menu: None,
        menu_hover: None,
        effects: panel::Effects::default(),
    }
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
    target.draw(frame(spec, palette, metrics, None))?;
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
/// V3 基准画同一份,数字才有可比性。
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
