//! Skia 候选窗的 GPU 层:Ganesh-D3D12 直连上下文 + DirectComposition 交换链。
//!
//! 只负责表面、出帧与提交;窗口、动效与命中都在 `ui_skia`。交换链交给
//! DComp 合成而不是 HWND 表面,圆角与淡入淡出靠逐像素 alpha(预乘)。
//! 离屏路径(`Offscreen`)不依赖窗口与 DComp,给没有桌面会话的环境留一条
//! 能验证出帧与像素的路。

use lueying_render::panel::{self, Metrics, PanelSpec, Palette};
use skia_safe::{
    Canvas, Color, ColorType, Font, ImageInfo, Rect, Surface,
    gpu::{
        self, BackendRenderTarget, DirectContext, Protected, SurfaceOrigin,
        d3d::{BackendContext, TextureResourceInfo},
    },
};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12CreateDevice, D3D12_COMMAND_QUEUE_DESC, D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES,
    D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
    D3D12_RESOURCE_FLAG_NONE, D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN,
    ID3D12CommandQueue, ID3D12Device, ID3D12Resource,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::{
    Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_NONE, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIAdapter1, IDXGIDevice, IDXGIFactory4, IDXGISwapChain1,
    IDXGISwapChain3,
};
use windows::core::Interface;

/// 交换链缓冲数;双缓冲配 flip 模型。
const BUFFER_COUNT: usize = 2;

/// 一帧的绘制输入;窗口层换算,管线照画。
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    pub spec: &'a PanelSpec,
    pub palette: &'a Palette,
    pub metrics: &'a Metrics,
    pub font: Option<&'a Font>,
    /// 整窗透明度(淡入/淡出)。
    pub opacity: f32,
    /// 候选列表动效(翻页滑动)。
    pub list: panel::ListFx,
    /// 页内悬停候选。
    pub hover: Option<usize>,
    /// 悬停的翻页箭头。
    pub pager: Option<panel::Hit>,
    /// 右键菜单左上角。
    pub menu: Option<(f32, f32)>,
    /// 悬停的菜单项。
    pub menu_hover: Option<usize>,
    /// 三个槽位的用户效果;为空就是素色。
    pub effects: panel::Effects<'a>,
}

/// 出帧路径的最小接口:画一帧、读回一个像素。
/// 窗口路径与离屏路径共用,自检对两条路径用同一套像素期望。
pub trait FrameTarget {
    fn draw(&mut self, f: Frame) -> Result<(), String>;
    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)>;
}

/// 把一帧画到画布上;两条出帧路径共用,像素结果一致。
fn paint(canvas: &Canvas, f: Frame) {
    let width = panel::panel_width(f.spec, f.metrics, f.font);
    let height = panel::panel_height(f.spec, f.metrics, f.font);
    canvas.clear(Color::TRANSPARENT);
    // 淡入/淡出作用整窗;圆角之外的透明像素保持透明。
    if f.opacity < 1.0 {
        canvas.save_layer_alpha_f(Rect::from_wh(width, height), f.opacity.clamp(0.0, 1.0));
    }
    panel::draw_chrome(
        canvas,
        width,
        height,
        f.spec,
        f.palette,
        f.metrics,
        f.font,
        f.pager,
        f.effects,
    );
    panel::draw_items(
        canvas,
        width,
        height,
        f.spec,
        f.palette,
        f.metrics,
        f.font,
        f.list,
        f.hover,
        f.effects,
    );
    if let Some((mx, my)) = f.menu {
        panel::draw_menu(
            canvas,
            mx,
            my,
            f.palette,
            f.metrics,
            f.font,
            f.menu_hover,
            f.effects,
        );
    }
    if f.opacity < 1.0 {
        canvas.restore();
    }
}

/// DComp 交换链 + Ganesh-D3D12 出帧管线;窗口尺寸变化时重建表面。
pub struct Pipeline {
    direct_context: DirectContext,
    swapchain: IDXGISwapChain3,
    dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    surfaces: Vec<(Surface, BackendRenderTarget)>,
    size: (u32, u32),
}

impl Pipeline {
    pub fn new(hwnd: HWND) -> Result<Self, String> {
        unsafe {
            let factory: IDXGIFactory4 =
                CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
            let (adapter, device) = pick_adapter(&factory)?;
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC::default())
                .map_err(|e| format!("CreateCommandQueue: {e}"))?;
            let direct_context = gpu::direct_contexts::make_d3d(
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
            // 只拿 DComp 做合成交换链,不建 DComp 表面,所以渲染设备可以不给。
            let dcomp: IDCompositionDevice = DCompositionCreateDevice2(None::<&IDXGIDevice>)
                .map_err(|e| format!("DCompositionCreateDevice2: {e}"))?;
            let target = dcomp
                .CreateTargetForHwnd(hwnd, true)
                .map_err(|e| format!("CreateTargetForHwnd: {e}"))?;
            let visual = dcomp
                .CreateVisual()
                .map_err(|e| format!("CreateVisual: {e}"))?;
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
                // 合成交换链只吃 flip 模型:FLIP_SEQUENTIAL + STRETCH 是硬性要求。
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                // 候选窗的圆角靠逐像素 alpha;DComp 只吃预乘。
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swapchain1: IDXGISwapChain1 = factory
                .CreateSwapChainForComposition(&queue, &desc, None)
                .map_err(|e| format!("CreateSwapChainForComposition: {e}"))?;
            let swapchain: IDXGISwapChain3 = swapchain1
                .cast()
                .map_err(|e| format!("cast SwapChain3: {e}"))?;
            let content: windows::core::IUnknown = swapchain
                .cast()
                .map_err(|e| format!("cast IUnknown: {e}"))?;
            visual
                .SetContent(&content)
                .map_err(|e| format!("SetContent: {e}"))?;
            target.SetRoot(&visual).map_err(|e| format!("SetRoot: {e}"))?;
            dcomp
                .Commit()
                .map_err(|e| format!("DComp 首次 Commit: {e}"))?;
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
    pub fn rebuild_surfaces(&mut self, w: u32, h: u32) -> Result<(), String> {
        if self.size == (w, h) && !self.surfaces.is_empty() {
            return Ok(());
        }
        // Skia 对后缓冲的引用先放掉,D3D12 才允许 ResizeBuffers。
        self.surfaces.clear();
        unsafe {
            self.swapchain
                .ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))
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
                    // 单 mip 纹理按 Skia 的约定填 1,填 0 会踩它的断言。
                    level_count: 1,
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

    pub fn present(&mut self) -> Result<(), String> {
        unsafe {
            let hr = self.swapchain.Present(0, DXGI_PRESENT::default());
            if hr.is_err() {
                return Err(format!("Present: 0x{:08X}", hr.0 as u32));
            }
            self.dcomp
                .Commit()
                .map_err(|e| format!("DComp Commit: {e}"))?;
        }
        Ok(())
    }
}

impl FrameTarget for Pipeline {
    fn draw(&mut self, f: Frame) -> Result<(), String> {
        let index = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize;
        let (surface, _) = self.surfaces.get_mut(index).ok_or("后缓冲表面缺失")?;
        paint(surface.canvas(), f);
        self.direct_context
            .flush_and_submit_surface(surface, None)
            .map_err(|e| format!("flush: {e}"))?;
        Ok(())
    }

    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        let index = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize;
        let (surface, _) = self.surfaces.get_mut(index)?;
        read_bgr(surface, x, y)
    }
}

/// 离屏渲染:Ganesh-D3D12 直接画进自建纹理,不需要窗口与 DComp。
/// 没有桌面会话(CI)或合成交换链建不起来时,仍能验证出帧与像素。
pub struct Offscreen {
    surface: Surface,
    /// 后端渲染目标要活到表面之后;Skia 只借用它,不接管。
    _target: BackendRenderTarget,
    _resource: ID3D12Resource,
    ctx: DirectContext,
}

impl Offscreen {
    pub fn new(w: u32, h: u32) -> Result<Self, String> {
        unsafe {
            let factory: IDXGIFactory4 =
                CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
            let (adapter, device) = pick_adapter(&factory)?;
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC::default())
                .map_err(|e| format!("CreateCommandQueue: {e}"))?;
            let mut ctx = gpu::direct_contexts::make_d3d(
                &BackendContext {
                    adapter,
                    device: device.clone(),
                    queue,
                    memory_allocator: None,
                    protected_context: Protected::No,
                },
                None,
            )
            .ok_or("创建 D3D12 DirectContext 失败")?;
            let resource = create_texture(&device, w, h)?;
            let target = gpu::backend_render_targets::make_d3d(
                (w as i32, h as i32),
                &TextureResourceInfo {
                    resource: resource.clone(),
                    alloc: None,
                    resource_state: D3D12_RESOURCE_STATE_COMMON,
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    sample_count: 1,
                    level_count: 1,
                    sample_quality_pattern: DXGI_STANDARD_MULTISAMPLE_QUALITY_PATTERN,
                    protected: Protected::No,
                },
            );
            let surface = gpu::surfaces::wrap_backend_render_target(
                &mut ctx,
                &target,
                SurfaceOrigin::TopLeft,
                ColorType::BGRA8888,
                None,
                None,
            )
            .ok_or("wrap_backend_render_target 失败")?;
            Ok(Self {
                surface,
                _target: target,
                _resource: resource,
                ctx,
            })
        }
    }
}

impl FrameTarget for Offscreen {
    fn draw(&mut self, f: Frame) -> Result<(), String> {
        paint(self.surface.canvas(), f);
        self.ctx
            .flush_and_submit_surface(&mut self.surface, None)
            .map_err(|e| format!("flush: {e}"))?;
        Ok(())
    }

    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        read_bgr(&mut self.surface, x, y)
    }
}

/// 建一张单 mip 的 D3D12 纹理,给离屏出帧当目标。
fn create_texture(device: &ID3D12Device, w: u32, h: u32) -> Result<ID3D12Resource, String> {
    unsafe {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: w as u64,
            Height: h,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_NONE,
            ..Default::default()
        };
        let mut resource: Option<ID3D12Resource> = None;
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
            .map_err(|e| format!("CreateCommittedResource: {e}"))?;
        resource.ok_or_else(|| "CreateCommittedResource 没有返回资源".to_string())
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
            let is_software = (DXGI_ADAPTER_FLAG(desc.Flags as _) & DXGI_ADAPTER_FLAG_SOFTWARE)
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

/// N32 小端内存字节序是 BGRA,读回 (B, G, R)。
pub fn read_bgr(surface: &mut Surface, x: i32, y: i32) -> Option<(u8, u8, u8)> {
    let (w, h) = (surface.width(), surface.height());
    let info = ImageInfo::new_n32_premul((w, h), None);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    if !surface.read_pixels(&info, &mut buf, info.min_row_bytes(), (0, 0)) {
        return None;
    }
    let at = ((y * w + x) * 4) as usize;
    Some((buf[at], buf[at + 1], buf[at + 2]))
}

/// 0xRRGGBB 转成与 [`read_bgr`] 同序的 (B, G, R),自检比较才不会错位。
pub fn rgb(rgb: u32) -> (u8, u8, u8) {
    (
        (rgb & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        ((rgb >> 16) & 0xff) as u8,
    )
}
