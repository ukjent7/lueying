//! 候选窗 V3 帧级测量:首帧、稳态帧耗时、SkSL 帧增量与常驻内存。
//!
//! `LUEYING_SKIA_BENCH=<报告路径>` 时随服务端启动跑一次:光栅(CPU)路径
//! 在本进程,Ganesh-D3D12(GPU)路径另起子进程,崩溃隔离与自检同款。
//! 报告是 Markdown,随 CI artifact 保留;「按 V3 数据决定默认渲染路径」
//! 的依据就是这份报告,决策记录在 docs/architecture.md。
//!
//! 只测绘制,不测上屏:交换链提交与分层窗口贴图跟桌面状态耦合,数字
//! 噪;两条路径的窗口层等价性由换手自检覆盖。内存报基准期间的工作集
//! 增量,把词典这类与渲染无关的常驻排除在外。

use std::time::{Duration, Instant};

use lueying_render::effect::{self, Slot, Uniforms};
use lueying_render::panel::{self, Effects, Metrics, Palette, PanelSpec};
use lueying_server::theme;
use skia_safe::Shader;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, IsWindow};

use crate::ui_skia_gpu::{Frame, FrameTarget, Offscreen, Raster};
use crate::ui_skia_selftest::{demo_spec, gpu_child, window_target};

/// 预热帧数:把字体缓存与着色器管线抬到稳态。
const WARMUP: usize = 10;
/// 稳态与 SkSL 各测的帧数;毫秒级 ×120,总时长可忽略。
const FRAMES: usize = 120;

/// 一条出帧路径的测量结果。
struct Timings {
    build_ms: f64,
    first_ms: f64,
    steady_avg_ms: f64,
    steady_max_ms: f64,
    sksl_avg_ms: f64,
}

/// 基准入口:CPU 本进程,GPU 子进程;报告写到 `report` 路径。
pub(super) fn run(report: &str) {
    let metrics = Metrics::standard();
    let spec = demo_spec();
    let palette = theme::palette(theme::colors_with(theme::Appearance::Light, 0));
    let memory_before = working_set_mib();
    let cpu = match raster_bench(&spec, &palette, &metrics) {
        Ok(timings) => {
            let memory = working_set_mib().zip(memory_before).map(|(a, b)| a - b);
            section("CPU(光栅)", &timings, memory)
        }
        Err(e) => format!("失败:{e}"),
    };
    // GPU 子进程的内存只含它自己,增量口径与本进程一致。
    let gpu = match gpu_child("LUEYING_SKIA_BENCH_GPU") {
        Ok(text) => text,
        Err(e) => format!("不可用:{e}"),
    };
    let text = format!(
        "# 候选窗 V3 帧级测量\n\n\
         面板内容与 Linux E2E 同一套(2 候选 + 拼音条);预热 {WARMUP} 帧,稳态与 \
         SkSL 各取 {FRAMES} 帧均值/最大;SkSL 用背景槽默认模板。内存是基准期间的 \
         工作集增量(含 Skia 与表面,不含词典);上屏(交换链提交、分层窗口贴图)\
         不计入,窗口层等价性由换手自检覆盖。\n\n{cpu}\n\n{gpu}\n"
    );
    if let Err(e) = std::fs::write(report, &text) {
        println!("SKIA 基准:写报告失败 {e}");
        return;
    }
    println!("SKIA 基准:报告已写 {report}");
}

/// GPU 基准(子进程):合成交换链建不起来就降级离屏;结果打到 stdout,
/// 父进程收进报告。GPU 管线在坏驱动上会崩,崩了只影响子进程。
pub(super) fn gpu_bench() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let metrics = Metrics::standard();
    let spec = demo_spec();
    let palette = theme::palette(theme::colors_with(theme::Appearance::Light, 0));
    let width = panel::panel_width(&spec, &metrics, None) as u32;
    let height = panel::panel_height(&spec, &metrics, None) as u32;
    let memory_before = working_set_mib();
    let outcome = {
        let build = Instant::now();
        let target = window_target(width, height);
        let build_ms = build.elapsed().as_secs_f64() * 1000.0;
        match target {
            Ok((mut pipe, hwnd)) => {
                let result = measure(&mut pipe, build_ms, &spec, &palette, &metrics)
                    .map(|t| (t, "GPU(Ganesh-D3D12,合成交换链)"));
                // 先放管线再销窗:DComp 目标还指着这个 HWND。
                drop(pipe);
                unsafe {
                    if IsWindow(Some(hwnd)).as_bool() {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                result
            }
            Err(e) => {
                println!("GPU 降级:合成交换链不可用({e}),改走离屏渲染");
                let start = Instant::now();
                match Offscreen::new(width, height) {
                    Ok(mut target) => {
                        let build_ms = start.elapsed().as_secs_f64() * 1000.0;
                        measure(&mut target, build_ms, &spec, &palette, &metrics)
                            .map(|t| (t, "GPU(Ganesh-D3D12,离屏渲染)"))
                    }
                    Err(e) => Err(format!("离屏渲染建管线 {e}")),
                }
            }
        }
    };
    match outcome {
        Ok((timings, title)) => {
            let memory = working_set_mib().zip(memory_before).map(|(a, b)| a - b);
            println!("{}", section(title, &timings, memory).trim_end());
        }
        Err(e) => println!("GPU 测量失败:{e}"),
    }
}

/// 光栅路径:建面计时 + 测量;CPU 基线不依赖桌面会话,任何环境都能出数。
fn raster_bench(
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
) -> Result<Timings, String> {
    let width = panel::panel_width(spec, metrics, None) as u32;
    let height = panel::panel_height(spec, metrics, None) as u32;
    let start = Instant::now();
    let mut target = Raster::new(width, height)?;
    let build_ms = start.elapsed().as_secs_f64() * 1000.0;
    measure(&mut target, build_ms, spec, palette, metrics)
}

/// 建面之后的所有测量:首帧、稳态、SkSL 增量。`build_ms` 是建面耗时,
/// 首帧 = 建面 + 首绘,分两行报。
fn measure(
    target: &mut dyn FrameTarget,
    build_ms: f64,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
) -> Result<Timings, String> {
    let width = panel::panel_width(spec, metrics, None);
    let height = panel::panel_height(spec, metrics, None);
    let first = Instant::now();
    target.draw(frame(spec, palette, metrics, None))?;
    let first_ms = build_ms + first.elapsed().as_secs_f64() * 1000.0;
    for _ in 0..WARMUP {
        target.draw(frame(spec, palette, metrics, None))?;
    }
    let (steady_avg_ms, steady_max_ms) = steady_frames(target, spec, palette, metrics, None)?;
    // SkSL:加载期编译一次,帧内只打包 uniform 生成 shader。
    let effect = effect::compile_background(effect::template(Slot::Background))?;
    let uniforms = Uniforms {
        size: [width, height],
        time: 0.0,
        anim: 1.0,
        accent: Uniforms::accent_from_rgb(palette.accent),
    };
    let shader = effect::make_shader(&effect, &uniforms).ok_or("效果没出 shader")?;
    let (sksl_avg_ms, _) = steady_frames(target, spec, palette, metrics, Some(&shader))?;
    Ok(Timings {
        build_ms,
        first_ms,
        steady_avg_ms,
        steady_max_ms,
        sksl_avg_ms,
    })
}

/// 连画 `FRAMES` 帧取均值与最大;`shader` 非空时帧里带背景槽效果。
fn steady_frames(
    target: &mut dyn FrameTarget,
    spec: &PanelSpec,
    palette: &Palette,
    metrics: &Metrics,
    shader: Option<&Shader>,
) -> Result<(f64, f64), String> {
    let mut sum = Duration::ZERO;
    let mut max = Duration::ZERO;
    for _ in 0..FRAMES {
        let start = Instant::now();
        target.draw(frame(spec, palette, metrics, shader))?;
        let elapsed = start.elapsed();
        sum += elapsed;
        max = max.max(elapsed);
    }
    let avg = sum.as_secs_f64() * 1000.0 / FRAMES as f64;
    Ok((avg, max.as_secs_f64() * 1000.0))
}

/// 一帧的绘制输入;与自检同款,无悬停无菜单的静态面板。
fn frame<'a>(
    spec: &'a PanelSpec,
    palette: &'a Palette,
    metrics: &'a Metrics,
    shader: Option<&'a Shader>,
) -> Frame<'a> {
    Frame {
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
        effects: Effects {
            background: shader,
            highlight: None,
            menu: None,
        },
    }
}

/// 报告小节;`memory` 是基准期间的工作集增量(MiB)。
fn section(title: &str, t: &Timings, memory: Option<f64>) -> String {
    let mut text = format!("### {title}\n\n| 项 | 值 |\n|---|---|\n");
    text.push_str(&format!("| 建面 | {:.1} ms |\n", t.build_ms));
    text.push_str(&format!("| 首帧(建面 + 首绘) | {:.1} ms |\n", t.first_ms));
    text.push_str(&format!("| 稳态均值 | {:.2} ms |\n", t.steady_avg_ms));
    text.push_str(&format!("| 稳态最大 | {:.2} ms |\n", t.steady_max_ms));
    text.push_str(&format!("| SkSL 稳态均值 | {:.2} ms |\n", t.sksl_avg_ms));
    match memory {
        Some(value) => text.push_str(&format!("| 工作集增量 | {value:.1} MiB |\n")),
        None => text.push_str("| 工作集增量 | 读不到 |\n"),
    }
    text
}

/// 进程工作集(MiB);读不到按 None,报告里如实标注。
fn working_set_mib() -> Option<f64> {
    let mut info = PROCESS_MEMORY_COUNTERS::default();
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    info.cb = size;
    unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), &mut info, size)
            .ok()
            .map(|_| info.WorkingSetSize as f64 / 1048576.0)
    }
}
