//! Skia 候选窗:Win32 无焦点弹出窗 + DirectComposition 交换链 + Ganesh-D3D12。
//!
//! `ui_backend=skia` 时启用(默认仍是 GPUI):独立线程自建消息泵,15ms
//! 轮询会话版本号;摆位、停靠、DPI 与全屏避让同 GPUI 路径共用 `ui_place`。
//! D3D12 优先挑硬件适配器,没有就退 WARP,建不出设备时放弃候选窗并写日志,
//! 输入本身不受影响。淡出、翻页滑动与鼠标交互是 Stage 2 的活;本阶段把
//! 窗口与出帧管线立起来,`LUEYING_SKIA_SELFTEST` 供 CI 自检整条管线。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lueying_protocol::SessionView;
use lueying_render::panel::{self, Item, Layout, Metrics, PanelSpec, Palette, PreeditBar};
use lueying_server::config::UiLayout;
use lueying_server::theme;
use skia_safe::{
    Color, ColorType, Font, FontStyle, FontMgr, Surface, ImageInfo,
    gpu::{
        self, BackendRenderTarget, DirectContext, Protected, SurfaceOrigin,
        d3d::{BackendContext, TextureResourceInfo},
    },
};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12CreateDevice, D3D12_COMMAND_QUEUE_DESC, D3D12_RESOURCE_STATE_COMMON, ID3D12CommandQueue,
    ID3D12Device, ID3D12Resource,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::{
    Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_NONE, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter1,
    IDXGIDevice, IDXGIFactory4, IDXGISwapChain1, IDXGISwapChain3,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, HWND_TOPMOST, IDC_ARROW,
    LoadCursorW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SWP_NOACTIVATE, SetWindowPos,
    TranslateMessage, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WS_VISIBLE,
};
use windows::core::{Interface, w};

use crate::imp::State;
use crate::ui_place::{self, CARET_GAP};

/// 交换链缓冲数;双缓冲配 flip 模型。
const BUFFER_COUNT: usize = 2;
/// 会话版本号轮询间隔;与 GPUI 路径一致,对打字反馈无感。
const POLL: Duration = Duration::from_millis(15);
/// 候选文字最大宽度(逻辑像素);与 GPUI 路径的 `MAX_WIDTH - 190` 一致。
const MAX_TEXT_WIDTH: f32 = 490.0;

/// 启动 Skia 候选窗线程;建窗或建管线失败只写日志,输入法照常工作。
pub(super) fn spawn(state: Arc<State>) {
    let spawned = std::thread::Builder::new()
        .name("skia-candidate".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| window_loop(state))).is_err()
            {
                println!("Skia 候选窗线程崩溃,后续组合不再有候选窗");
            }
        });
    if let Err(e) = spawned {
        println!("Skia 候选窗线程启动失败: {e}");
    }
}

fn window_loop(state: Arc<State>) {
    // DComp 与窗口消息都归这条线程;COM 已按别的 apartment 初始化过时忽略。
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            println!("Skia 候选窗建窗失败: {e}");
            return;
        }
    };
    let mut pipe = match Pipeline::new(hwnd) {
        Ok(pipe) => pipe,
        Err(e) => {
            println!("Skia 候选窗 D3D12 管线初始化失败: {e}");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        }
    };
    println!("Skia 候选窗就绪:Ganesh-D3D12 + DirectComposition");
    let font = panel_font();
    // 上下朝向的滞回状态;组合结束/停靠时在 place_and_draw 里重置。
    let mut above: Option<bool> = None;
    let mut last = 0u64;
    let mut next = Instant::now();
    loop {
        // 无焦点窗口的消息量很小,每轮顺手排干;真正的节奏靠 5ms 睡眠。
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if Instant::now() >= next {
            next = (next + POLL).max(Instant::now());
            let version = state.version.load(Ordering::Relaxed);
            if version != last {
                last = version;
                place_and_draw(&state, &mut pipe, hwnd, font.as_ref(), &mut above);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 组合变化后重新摆位并出帧;无内容或全屏避让时停靠出屏,零 GPU 工作。
fn place_and_draw(
    state: &State,
    pipe: &mut Pipeline,
    hwnd: HWND,
    font: Option<&Font>,
    above: &mut Option<bool>,
) {
    let Some(view) = ui_place::snapshot(state) else {
        *above = None;
        let (x, y) = ui_place::park_point();
        ui_place::park(hwnd, x, y);
        return;
    };
    let (layout, ui_font, accent) = ui_place::ui_options(state);
    let appearance = theme::Appearance::of_system();
    let palette = palette_of(theme::colors_with(appearance, accent));
    let host_pid = (state.active.load(Ordering::Relaxed) >> 32) as u32;
    let (x, y, w, h, spec, metrics) = match view.caret {
        Some((l, t, r, b)) => {
            let left = l.min(r);
            // 按光标所在显示器算 DPI;停靠屏的缩放与本屏无关。
            let scale = ui_place::scale_at(left, b);
            let metrics = Metrics::scaled(ui_font.factor() * scale);
            let mut spec = spec_of(&view, layout);
            spec.max_text_width = MAX_TEXT_WIDTH * ui_font.factor() * scale;
            let mut w = panel::panel_width(&spec, &metrics, font) as i32;
            let h = panel::panel_height(&spec, &metrics, font) as i32;
            let work = ui_place::work_area_at(left, b);
            w = w.min((work.right - work.left).max(1));
            // 同一次组合内优先留在已选的一侧;只有这一侧放不下才翻边。
            // 副屏可以位于主屏上方,`top` 是负数,不能直接 `.max(0)`。
            let gap = (CARET_GAP * scale) as i32;
            let below = b + gap;
            let over = t - h - gap;
            let fits_below = below + h <= work.bottom;
            let fits_above = over >= work.top;
            let prefer_above = match *above {
                Some(previous) if previous && fits_above => true,
                Some(previous) if !previous && fits_below => false,
                _ => !fits_below && fits_above,
            };
            *above = Some(prefer_above);
            let y = (if prefer_above { over } else { below })
                .clamp(work.top, (work.bottom - h).max(work.top));
            let x = left.clamp(work.left, (work.right - w).max(work.left));
            // 独占全屏/无边框全屏里不弹候选窗,否则会盖在游戏画面上。
            let hidden = ui_place::foreground_fullscreen_at(left, b, host_pid);
            let w = if hidden { 0 } else { w };
            (x, y, w, h, spec, metrics)
        }
        None => {
            *above = None;
            (0, 0, 0, 0, spec_of(&view, layout), Metrics::standard())
        }
    };
    if w == 0 {
        let (park_x, park_y) = ui_place::park_point();
        ui_place::park(hwnd, park_x, park_y);
        return;
    }
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
    }
    if let Err(e) = pipe.rebuild_surfaces(w as u32, h as u32) {
        println!("Skia 交换链调整失败: {e}");
        return;
    }
    if let Err(e) = pipe.frame(&spec, &palette, &metrics, font) {
        println!("Skia 出帧失败,丢帧: {e}");
        return;
    }
    if let Err(e) = pipe.present() {
        println!("Skia 呈现失败: {e}");
    }
}

/// 会话视图转面板输入;页切片、选中态与页码标签与 GPUI 路径同一套语义。
fn spec_of(view: &SessionView, layout: UiLayout) -> PanelSpec {
    let ps = view.page_size.max(1);
    let start = view.page * ps;
    let end = (start + ps).min(view.candidates.len());
    let items = (start..end)
        .map(|i| Item {
            text: view.candidates[i].clone(),
            hint: view.hints.get(i).cloned().unwrap_or_default(),
            selected: i == view.highlight,
        })
        .collect();
    let pages = view.candidates.len().div_ceil(ps).max(1);
    let page_label = if pages > 1 {
        format!("{}/{}", view.page + 1, pages)
    } else {
        String::new()
    };
    PanelSpec {
        width: 0.0,
        // 摆位时按字号档 × DPI 补上;这里先置零。
        max_text_width: 0.0,
        preedit: PreeditBar {
            text: view.raw.clone(),
            page_label,
        },
        items,
        layout: if matches!(layout, UiLayout::Horizontal) {
            Layout::Wrap
        } else {
            Layout::Column
        },
    }
}

/// `theme::Colors` → 渲染令牌;候选窗底色是 surface,bg 是设置窗的。
fn palette_of(c: theme::Colors) -> Palette {
    Palette {
        bg: c.surface,
        surface_alt: c.surface_alt,
        border: c.border,
        text: c.text,
        text_secondary: c.text_secondary,
        text_tertiary: c.text_tertiary,
        accent: c.accent,
        accent_text: c.accent_text,
        selected_bg: c.selected_bg,
        selected_text: c.selected_text,
    }
}

/// DirectWrite 默认字体管理器里取微软雅黑;取不到时走无字体路径(只画背景)。
fn panel_font() -> Option<Font> {
    let face = FontMgr::new()
        .match_family_style("Microsoft YaHei UI", FontStyle::normal())?;
    Some(Font::from_typeface(face, 17.0))
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// 无焦点置顶工具窗;样式在创建时给足,停靠点出生,避免 Alt+Tab 里露脸。
fn create_window() -> Result<HWND, String> {
    unsafe {
        let hmodule = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
        let class = w!("lueying-skia-candidate");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(hmodule.0),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| format!("LoadCursorW: {e}"))?,
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err("RegisterClassW 失败".into());
        }
        let (park_x, park_y) = ui_place::park_point();
        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("lueying"),
            WS_POPUP | WS_VISIBLE,
            park_x,
            park_y,
            360,
            76,
            None,
            None,
            Some(HINSTANCE(hmodule.0)),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        Ok(hwnd)
    }
}

/// DComp 交换链 + Ganesh-D3D12 出帧管线;窗口尺寸变化时重建表面。
struct Pipeline {
    direct_context: DirectContext,
    swapchain: IDXGISwapChain3,
    dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    surfaces: Vec<(Surface, BackendRenderTarget)>,
    size: (u32, u32),
}

impl Pipeline {
    fn new(hwnd: HWND) -> Result<Self, String> {
        unsafe {
            let factory: IDXGIFactory4 =
                CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
            let (adapter, device) = pick_adapter(&factory)?;
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC::default())
                .map_err(|e| format!("CreateCommandQueue: {e}"))?;
            let direct_context =
                gpu::direct_contexts::make_d3d(
                    &BackendContext {
                        adapter,
                        device: device.clone(),
                        queue: queue.clone(),
                        memory_allocator: None,
                        protected_context: Protected::No,
                    },
                    None,
                )
                .ok_or("创建 D3D12 DirectContext 失败")?;
            // DComp 要 IDXGIDevice;从 D3D12 设备 cast 而不是另建。
            let dxgi_device: IDXGIDevice = device.cast().map_err(|e| format!("cast IDXGIDevice: {e}"))?;
            let dcomp: IDCompositionDevice =
                DCompositionCreateDevice(&dxgi_device).map_err(|e| format!("DCompositionCreateDevice: {e}"))?;
            let target = dcomp
                .CreateTargetForHwnd(hwnd, true)
                .map_err(|e| format!("CreateTargetForHwnd: {e}"))?;
            let visual = dcomp.CreateVisual().map_err(|e| format!("CreateVisual: {e}"))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: 360,
                Height: 76,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: BUFFER_COUNT as u32,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                // 候选窗的圆角与投影靠逐像素 alpha;DComp 只吃预乘。
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swapchain1: IDXGISwapChain1 = factory
                .CreateSwapChainForComposition(&queue, &desc, None)
                .map_err(|e| format!("CreateSwapChainForComposition: {e}"))?;
            let swapchain: IDXGISwapChain3 = swapchain1.cast().map_err(|e| format!("cast SwapChain3: {e}"))?;
            let content: windows::core::IUnknown = swapchain.cast().map_err(|e| format!("cast IUnknown: {e}"))?;
            visual.SetContent(&content).map_err(|e| format!("SetContent: {e}"))?;
            target.SetRoot(&visual).map_err(|e| format!("SetRoot: {e}"))?;
            dcomp.Commit().map_err(|e| format!("DComp 首次 Commit: {e}"))?;
            let mut pipe = Self {
                direct_context,
                swapchain,
                dcomp,
                _target: target,
                _visual: visual,
                surfaces: Vec::new(),
                size: (0, 0),
            };
            pipe.rebuild_surfaces(360, 76)?;
            Ok(pipe)
        }
    }

    /// 窗口尺寸变化时改交换链大小并重新包裹两块后缓冲。
    fn rebuild_surfaces(&mut self, w: u32, h: u32) -> Result<(), String> {
        if self.size == (w, h) && !self.surfaces.is_empty() {
            return Ok(());
        }
        // Skia 对后缓冲的引用先放掉,D3D12 才允许 ResizeBuffers。
        self.surfaces.clear();
        unsafe {
            self.swapchain
                .ResizeBuffers(
                    0,
                    w,
                    h,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )
                .map_err(|e| format!("ResizeBuffers({w}x{h}): {e}"))?;
        }
        let mut surfaces = Vec::with_capacity(BUFFER_COUNT);
        for i in 0..BUFFER_COUNT {
            let resource: ID3D12Resource = self
                .swapchain
                .GetBuffer(i as u32)
                .map_err(|e| format!("GetBuffer({i}): {e}"))?;
            let target = gpu::backend_render_targets::make_d3d(
                (w as i32, h as i32),
                &TextureResourceInfo {
                    resource,
                    alloc: None,
                    resource_state: D3D12_RESOURCE_STATE_COMMON,
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    sample_count: 1,
                    level_count: 0,
                    sample_quality_pattern: DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN,
                    protected: Protected::No,
                },
            );
            // D3D 的帧缓冲原点在左上,与 Skia 设备坐标一致。
            let surface = gpu::surfaces::wrap_backend_render_target(
                &mut self.direct_context,
                &target,
                SurfaceOrigin::TopLeft,
                ColorType::BGRA8888,
                None,
                None,
            )
            .ok_or("wrap_backend_render_target 失败")?;
            surfaces.push((surface, target));
        }
        self.surfaces = surfaces;
        self.size = (w, h);
        Ok(())
    }

    /// 画一帧并提交 GPU;返回当前后缓冲表面供读回,随后调用 [`Self::present`]。
    fn frame(
        &mut self,
        spec: &PanelSpec,
        palette: &Palette,
        metrics: &Metrics,
        font: Option<&Font>,
    ) -> Result<&mut Surface, String> {
        let index = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize;
        let (surface, _) = self
            .surfaces
            .get_mut(index)
            .ok_or("后缓冲表面缺失")?;
        surface.canvas().clear(Color::TRANSPARENT);
        panel::draw(surface.canvas(), spec, palette, metrics, font);
        self.direct_context
            .flush_and_submit_surface(surface, None)
            .map_err(|e| format!("flush: {e}"))?;
        Ok(surface)
    }

    fn present(&mut self) -> Result<(), String> {
        unsafe {
            let hr = self.swapchain.Present(0, DXGI_PRESENT::default());
            if hr.is_err() {
                return Err(format!("Present: 0x{:08X}", hr.0 as u32));
            }
            self.dcomp.Commit().map_err(|e| format!("DComp Commit: {e}"))?;
        }
        Ok(())
    }
}

/// 硬件适配器优先,没有可用的再退 WARP(远程桌面/虚拟机)。
fn pick_adapter(factory: &IDXGIFactory4) -> Result<(IDXGIAdapter1, ID3D12Device), String> {
    unsafe {
        let mut software: Option<(IDXGIAdapter1, ID3D12Device)> = None;
        let mut i = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            let is_software = DXGI_ADAPTER_FLAG(desc.Flags as _) & DXGI_ADAPTER_FLAG_SOFTWARE
                != DXGI_ADAPTER_FLAG_NONE;
            let mut device: Option<ID3D12Device> = None;
            if D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device).is_ok()
                && let Some(device) = device
            {
                if is_software {
                    software = Some((adapter, device));
                } else {
                    return Ok((adapter, device));
                }
            }
        }
        software.ok_or_else(|| "没有可用的 D3D12 适配器(含 WARP)".to_string())
    }
}

/// CI 自检:建窗、Ganesh-D3D12 出一帧面板、读回像素断言,结论写日志。
/// 与 Linux 光栅 E2E 同一套期望值(无字体估算路径),失败由 win-e2e 断言。
pub(super) fn selftest() {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let hwnd = match create_window() {
        Ok(hwnd) => hwnd,
        Err(e) => {
            println!("SKIA 自检失败:建窗 {e}");
            return;
        }
    };
    let mut pipe = match Pipeline::new(hwnd) {
        Ok(pipe) => pipe,
        Err(e) => {
            println!("SKIA 自检失败:管线 {e}");
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        }
    };
    // 固定 100% 缩放与无字体估算路径,像素期望与 Linux E2E 完全一致。
    let metrics = Metrics::standard();
    let spec = demo_spec();
    let width = panel::panel_width(&spec, &metrics, None);
    let height = panel::panel_height(&spec, &metrics, None);
    let palette = palette_of(theme::colors_with(theme::Appearance::Light, 0));
    let outcome = (|| -> Result<(), String> {
        pipe.rebuild_surfaces(width as u32, height as u32)?;
        let surface = pipe.frame(&spec, &palette, &metrics, None)?;
        let expected_bg = rgb(palette.bg);
        let expected_bar = rgb(palette.surface_alt);
        let bg = read_bgr(surface, 5, 60).ok_or("像素读回失败(背景)")?;
        if bg != expected_bg {
            return Err(format!("背景像素 {bg:?} 期望 {expected_bg:?}"));
        }
        let bar = read_bgr(surface, 5, 10).ok_or("像素读回失败(拼音条)")?;
        if bar != expected_bar {
            return Err(format!("拼音条像素 {bar:?} 期望 {expected_bar:?}"));
        }
        pipe.present()
    })();
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    match outcome {
        Ok(()) => println!(
            "SKIA 自检通过:Ganesh-D3D12 帧读回一致({}x{})",
            width as u32, height as u32
        ),
        Err(e) => println!("SKIA 自检失败:{e}"),
    }
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
