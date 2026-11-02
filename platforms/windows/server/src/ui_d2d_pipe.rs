//! 候选窗的 D2D 管线:D3D11 设备 + Direct2D 设备上下文 + DirectComposition 合成交换链。
//!
//! 表面、出帧与提交在这里;窗口、动效与命中在 `ui_d2d`。交换链交给 DComp
//! 合成,圆角与淡入淡出靠逐像素 alpha(预乘)。没有硬件适配器时退到 WARP
//! (软件 D3D11,D2D 官方支持),同一套代码路径,画面一致;设备被系统摘掉
//! 就地整条重建,窗口不用换。离屏路径(`Offscreen`)不依赖窗口与 DComp,
//! 给没有桌面会话的环境留一条能验证出帧与像素的路。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lueying_render::panel::{self, Hit, ListFx, Measure, Metrics, Palette, PanelSpec};
use lueying_scene::{Body, EffectOut, Fill, Item as SceneItem, Layer as SceneLayer, Shape};
use windows::Win32::Foundation::{GENERIC_READ, HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_OVER,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1GaussianBlur, CLSID_D2D1Shadow, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION,
    D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1,
    D2D1_PROPERTY_TYPE_FLOAT, D2D1_ROUNDED_RECT, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION,
    D2D1_SHADOW_PROP_COLOR, D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext,
    ID2D1Effect, ID2D1Factory1, ID2D1Image, ID2D1Layer, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteFontCollection, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::{
    Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG, DXGI_ADAPTER_FLAG_NONE, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIFactory4,
    IDXGISwapChain1, IDXGISwapChain3,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapDecoder,
    IWICBitmapFrameDecode, IWICFormatConverter, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{Interface, w};
use windows_numerics::{Matrix3x2, Vector2};

/// 交换链缓冲数;双缓冲配 flip 模型。
const BUFFER_COUNT: usize = 2;

/// 一帧的绘制输入;窗口层换算,管线照画。
pub(super) struct Frame<'a> {
    pub spec: &'a PanelSpec,
    pub palette: &'a Palette,
    pub metrics: Metrics,
    /// 整窗透明度(淡入/淡出)。
    pub opacity: f32,
    /// 候选列表动效(翻页滑动)。
    pub list: ListFx,
    /// 页内悬停候选。
    pub hover: Option<usize>,
    /// 悬停的翻页箭头。
    pub pager: Option<Hit>,
    /// 右键菜单左上角。
    pub menu: Option<(f32, f32)>,
    /// 悬停的菜单项。
    pub menu_hover: Option<usize>,
    /// 场景图元;BelowText 在前,above-text 在后(运行时已排好)。
    pub scene: &'a [SceneItem],
    /// 场景素材目录;图片与精灵按它解析相对路径。
    pub scene_dir: &'a Path,
}

/// 场景素材查询;窗口路径读磁盘,离屏自检没有素材。
pub(super) trait SceneImages {
    fn bitmap(&self, dir: &Path, src: &str) -> Option<ID2D1Bitmap1>;
}

/// 出帧路径的最小接口:画一帧、读回一个像素。
/// 窗口路径与离屏路径共用,自检对两条路径用同一套像素期望。
pub(super) trait Target {
    fn paint_frame(&mut self, f: &Frame, text: &Text) -> Result<(), String>;
    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)>;
}

/// 文字引擎:DirectWrite 工厂 + 按字号缓存的文字格式;布局与绘制共用,
/// 量出来的窗才装得下画出来的字。
pub(super) struct Text {
    dwrite: IDWriteFactory,
    formats: RefCell<HashMap<u32, IDWriteTextFormat>>,
}

impl Text {
    pub(super) fn new() -> Result<Self, String> {
        Ok(Self {
            dwrite: unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) }
                .map_err(|e| format!("DWriteCreateFactory: {e}"))?,
            formats: RefCell::new(HashMap::new()),
        })
    }

    /// 指定字号的文字格式;系统里没有微软雅黑时 DirectWrite 自动退到默认字型。
    fn format(&self, size: f32) -> Result<IDWriteTextFormat, String> {
        let key = size.to_bits();
        if let Some(format) = self.formats.borrow().get(&key) {
            return Ok(format.clone());
        }
        let format = unsafe {
            self.dwrite
                .CreateTextFormat(
                    w!("Microsoft YaHei UI"),
                    None::<&IDWriteFontCollection>,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("zh-CN"),
                )
                .map_err(|e| format!("CreateTextFormat({size}): {e}"))?
        };
        unsafe {
            format
                .SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)
                .map_err(|e| format!("SetParagraphAlignment: {e}"))?;
            format
                .SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)
                .map_err(|e| format!("SetWordWrapping: {e}"))?;
        }
        self.formats.borrow_mut().insert(key, format.clone());
        Ok(format)
    }

    fn width_of(&self, text: &str, size: f32) -> f32 {
        let fallback = size * Metrics::text_units(text);
        let Ok(format) = self.format(size) else {
            return fallback;
        };
        let utf16: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let Ok(layout) =
                self.dwrite
                    .CreateTextLayout(&utf16, &format, f32::MAX / 4.0, f32::MAX / 4.0)
            else {
                return fallback;
            };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_err() {
                return fallback;
            }
            metrics.widthIncludingTrailingWhitespace
        }
    }
}

impl Measure for Text {
    fn width(&self, text: &str, size: f32) -> f32 {
        self.width_of(text, size)
    }
}

/// 共享的设备核心:D3D11 + D2D 设备上下文 + DXGI 工厂。窗口与离屏同源。
struct Core {
    factory: IDXGIFactory4,
    d2d: ID2D1Factory1,
    d3d: ID3D11Device,
    context: ID3D11DeviceContext,
    dc: ID2D1DeviceContext,
}

impl Core {
    fn new() -> Result<Self, String> {
        unsafe {
            let factory: IDXGIFactory4 =
                CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
            let adapter = pick_adapter(&factory)?;
            // 有适配器对象就必须配 UNKNOWN 驱动类型;WARP 由适配器自己代表。
            let mut d3d: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                // Direct2D 的设备必须支持 BGRA;WARP 与部分集显缺了这个标志
                // 会让 ID2D1Factory1::CreateDevice 直接返回 E_INVALIDARG。
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                Some(&mut context),
            )
            .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
            let d3d = d3d.ok_or("D3D11CreateDevice 没有返回设备")?;
            let context = context.ok_or("D3D11CreateDevice 没有返回上下文")?;
            let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .map_err(|e| format!("D2D1CreateFactory: {e}"))?;
            let dxgi: windows::Win32::Graphics::Dxgi::IDXGIDevice =
                d3d.cast().map_err(|e| format!("取 DXGI 设备: {e}"))?;
            let device: ID2D1Device = d2d
                .CreateDevice(&dxgi)
                .map_err(|e| format!("D2D CreateDevice: {e}"))?;
            let dc = device
                .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
                .map_err(|e| format!("CreateDeviceContext: {e}"))?;
            Ok(Self {
                factory,
                d2d,
                d3d,
                context,
                dc,
            })
        }
    }
}

/// 硬件适配器优先;没有硬件就用软件适配器(WARP)。D2D 对软件渲染是
/// 官方支持配置,自检在无 GPU 的 CI 上也走它。
fn pick_adapter(
    factory: &IDXGIFactory4,
) -> Result<windows::Win32::Graphics::Dxgi::IDXGIAdapter1, String> {
    unsafe {
        let mut software = None;
        let mut i = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            let is_software = (DXGI_ADAPTER_FLAG(desc.Flags as _) & DXGI_ADAPTER_FLAG_SOFTWARE)
                != DXGI_ADAPTER_FLAG_NONE;
            if is_software {
                software = Some(adapter);
            } else {
                return Ok(adapter);
            }
        }
        software.ok_or_else(|| "既没有硬件也没有软件适配器".to_string())
    }
}

/// DComp 交换链 + D2D 出帧管线;窗口尺寸变化时重建表面。
pub(super) struct Pipeline {
    core: Core,
    swapchain: IDXGISwapChain3,
    dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    bitmaps: Vec<ID2D1Bitmap1>,
    images: RefCell<HashMap<PathBuf, Option<ID2D1Bitmap1>>>,
    size: (u32, u32),
}

impl Pipeline {
    pub(super) fn new(hwnd: HWND) -> Result<Self, String> {
        let core = Core::new()?;
        unsafe {
            let dxgi: windows::Win32::Graphics::Dxgi::IDXGIDevice =
                core.d3d.cast().map_err(|e| format!("取 DXGI 设备: {e}"))?;
            // 只拿 DComp 做合成交换链,不建 DComp 表面,渲染设备跟着 D3D11 走。
            let dcomp: IDCompositionDevice = DCompositionCreateDevice2(&dxgi)
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
            let swapchain1: IDXGISwapChain1 = core
                .factory
                .CreateSwapChainForComposition(&core.d3d, &desc, None)
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
            target
                .SetRoot(&visual)
                .map_err(|e| format!("SetRoot: {e}"))?;
            dcomp
                .Commit()
                .map_err(|e| format!("DComp 首次 Commit: {e}"))?;
            let mut pipe = Self {
                core,
                swapchain,
                dcomp,
                _target: target,
                _visual: visual,
                bitmaps: Vec::new(),
                images: RefCell::new(HashMap::new()),
                size: (0, 0),
            };
            pipe.rebuild_surfaces(360, 76)?;
            Ok(pipe)
        }
    }

    /// 设备是否已被系统摘掉(驱动重启、GPU 挂起)。摘掉之后整条管线重建。
    pub(super) fn device_removed(&self) -> bool {
        unsafe { self.core.d3d.GetDeviceRemovedReason().is_err() }
    }

    /// 窗口尺寸变化时改交换链大小并重新包裹两块后缓冲。
    pub(super) fn rebuild_surfaces(&mut self, w: u32, h: u32) -> Result<(), String> {
        if self.size == (w, h) && !self.bitmaps.is_empty() {
            return Ok(());
        }
        // D2D 对后缓冲的引用先放掉,DXGI 才允许 ResizeBuffers。
        self.bitmaps.clear();
        self.images.borrow_mut().clear();
        unsafe {
            self.swapchain
                .ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))
                .map_err(|e| format!("ResizeBuffers({w}x{h}): {e}"))?;
            for i in 0..BUFFER_COUNT as u32 {
                let surface: windows::Win32::Graphics::Dxgi::IDXGISurface = self
                    .swapchain
                    .GetBuffer(i)
                    .map_err(|e| format!("GetBuffer({i}): {e}"))?;
                let bitmap = self
                    .core
                    .dc
                    .CreateBitmapFromDxgiSurface(&surface, None)
                    .map_err(|e| format!("CreateBitmapFromDxgiSurface: {e}"))?;
                self.bitmaps.push(bitmap);
            }
        }
        self.size = (w, h);
        Ok(())
    }

    pub(super) fn present(&mut self) -> Result<(), String> {
        unsafe {
            let hr = self.swapchain.Present(1, DXGI_PRESENT::default());
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

impl SceneImages for Pipeline {
    /// 加载场景素材;PNG 由系统 WIC 解码,不增加依赖。缺文件返回 None,
    /// 节点整颗跳过,候选窗照常。
    fn bitmap(&self, dir: &Path, src: &str) -> Option<ID2D1Bitmap1> {
        let key = dir.join(src);
        if let Some(cached) = self.images.borrow().get(&key) {
            return cached.clone();
        }
        let loaded = decode_bitmap(&self.core.dc, &key);
        if loaded.is_none() {
            println!("场景素材缺文件或解不开:{}", key.display());
        }
        self.images.borrow_mut().insert(key, loaded.clone());
        loaded
    }
}

/// 用 WIC 把图片解成 32bpp 预乘 BGRA 的 D2D 位图。
fn decode_bitmap(dc: &ID2D1DeviceContext, path: &Path) -> Option<ID2D1Bitmap1> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(
            &CLSID_WICImagingFactory,
            None::<&windows::core::IUnknown>,
            CLSCTX_INPROC_SERVER,
        )
        .ok()?;
        let decoder: IWICBitmapDecoder = wic
            .CreateDecoderFromFilename(
                &windows::core::HSTRING::from(path.as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .ok()?;
        let frame: IWICBitmapFrameDecode = decoder.GetFrame(0).ok()?;
        let converter: IWICFormatConverter = wic.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        dc.CreateBitmapFromWicBitmap(&converter, None).ok()
    }
}

impl Target for Pipeline {
    fn paint_frame(&mut self, f: &Frame, text: &Text) -> Result<(), String> {
        let index = unsafe { self.swapchain.GetCurrentBackBufferIndex() } as usize;
        let bitmap = self.bitmaps.get(index).ok_or("后缓冲位图缺失")?.clone();
        let dc = self.core.dc.clone();
        let factory = self.core.d2d.clone();
        unsafe {
            dc.SetTarget(&bitmap);
            dc.BeginDraw();
            paint(&dc, &factory, f, text, &bitmap, &*self);
            dc.EndDraw(None, None)
        }
        .map_err(|e| format!("EndDraw: {e}"))
    }

    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        let index = unsafe { self.swapchain.GetCurrentBackBufferIndex() };
        let texture: ID3D11Texture2D = unsafe { self.swapchain.GetBuffer(index) }.ok()?;
        read_bgr(&self.core.d3d, &self.core.context, &texture, x, y)
    }
}

/// 离屏渲染:D2D 画进自建 D3D11 纹理,不需要窗口与 DComp。
/// 没有桌面会话(CI)或合成交换链建不起来时,仍能验证出帧与像素。
pub(super) struct Offscreen {
    core: Core,
    texture: ID3D11Texture2D,
    bitmap: ID2D1Bitmap1,
}

impl Offscreen {
    pub(super) fn new(w: u32, h: u32) -> Result<Self, String> {
        let core = Core::new()?;
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut texture: Option<ID3D11Texture2D> = None;
            core.d3d
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .map_err(|e| format!("CreateTexture2D: {e}"))?;
            let texture = texture.ok_or("CreateTexture2D 没有返回纹理")?;
            let surface: windows::Win32::Graphics::Dxgi::IDXGISurface = texture
                .cast()
                .map_err(|e| format!("纹理转 DXGI 表面: {e}"))?;
            let bitmap = core
                .dc
                .CreateBitmapFromDxgiSurface(&surface, None)
                .map_err(|e| format!("CreateBitmapFromDxgiSurface: {e}"))?;
            Ok(Self {
                core,
                texture,
                bitmap,
            })
        }
    }
}

impl SceneImages for Offscreen {
    /// 离屏自检没有场景素材;图片与精灵节点整体跳过。
    fn bitmap(&self, _dir: &Path, _src: &str) -> Option<ID2D1Bitmap1> {
        None
    }
}

impl Target for Offscreen {
    fn paint_frame(&mut self, f: &Frame, text: &Text) -> Result<(), String> {
        let dc = self.core.dc.clone();
        let factory = self.core.d2d.clone();
        let bitmap = self.bitmap.clone();
        unsafe {
            dc.SetTarget(&bitmap);
            dc.BeginDraw();
            paint(&dc, &factory, f, text, &bitmap, &*self);
            dc.EndDraw(None, None)
        }
        .map_err(|e| format!("EndDraw: {e}"))
    }

    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        read_bgr(&self.core.d3d, &self.core.context, &self.texture, x, y)
    }
}

/// N32 预乘小端内存字节序是 BGRA,读回 (B, G, R)。
fn read_bgr(
    d3d: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    x: i32,
    y: i32,
) -> Option<(u8, u8, u8)> {
    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        texture.GetDesc(&mut desc);
        let staging_desc = D3D11_TEXTURE2D_DESC {
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..desc
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        d3d.CreateTexture2D(&staging_desc, None, Some(&mut staging))
            .ok()?;
        let staging = staging?;
        context.CopyResource(&staging, texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .ok()?;
        let at = y as usize * mapped.RowPitch as usize + x as usize * 4;
        let bytes = std::slice::from_raw_parts(mapped.pData as *const u8, at + 4);
        let pixel = (bytes[at], bytes[at + 1], bytes[at + 2]);
        context.Unmap(&staging, 0);
        Some(pixel)
    }
}

/// 场景素材的来源分辨率:@2x/@3x 写进文件名,没写按 1x。
fn asset_scale(src: &str) -> f32 {
    let lower = src.to_ascii_lowercase();
    if lower.contains("@3x") {
        3.0
    } else if lower.contains("@2x") {
        2.0
    } else {
        1.0
    }
}

/// 0xRRGGBB 转成与 [`read_bgr`] 同序的 (B, G, R),自检比较才不会错位。
pub(super) fn rgb(rgb: u32) -> (u8, u8, u8) {
    (
        (rgb & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        ((rgb >> 16) & 0xff) as u8,
    )
}

/// 0xRRGGBB 转 D2D 颜色。
fn color(rgb: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((rgb >> 16) & 0xff) as f32 / 255.0,
        g: ((rgb >> 8) & 0xff) as f32 / 255.0,
        b: (rgb & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// 场景配色转主题色。
fn scene_fill(fill: Fill, palette: &Palette) -> u32 {
    match fill {
        Fill::Palette(name) => match name {
            lueying_scene::PaletteName::Bg => palette.bg,
            lueying_scene::PaletteName::Border => palette.border,
            lueying_scene::PaletteName::Text => palette.text,
            lueying_scene::PaletteName::TextSecondary => palette.text_secondary,
            lueying_scene::PaletteName::TextTertiary => palette.text_tertiary,
            lueying_scene::PaletteName::Accent => palette.accent,
            lueying_scene::PaletteName::OnAccent => palette.on_accent,
            lueying_scene::PaletteName::Danger => palette.danger,
            lueying_scene::PaletteName::AccentText => palette.accent_text,
            lueying_scene::PaletteName::SelectedBg => palette.selected_bg,
            lueying_scene::PaletteName::SelectedText => palette.selected_text,
            lueying_scene::PaletteName::Hover => palette.hover,
        },
        Fill::Rgba(rgb) => rgb,
    }
}

fn rrect(rect: panel::Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        },
        radiusX: radius,
        radiusY: radius,
    }
}

fn d2d_rect(rect: panel::Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    }
}

/// 特效录制结果:按 `f.scene` 下标对位的模糊/阴影特效图。
struct Baked {
    blur: Vec<Option<ID2D1Effect>>,
    /// 阴影只出影子,本体照常画。
    shadow: Vec<Option<ID2D1Effect>>,
    /// 菜单投影;菜单位置固定在 `f.menu`。
    menu: Option<ID2D1Effect>,
}

impl Baked {
    fn empty() -> Self {
        Self {
            blur: Vec::new(),
            shadow: Vec::new(),
            menu: None,
        }
    }
}

/// 画一帧完整画面;窗口与离屏两条路径共用,像素结果一致。
///
/// 场景图元的坐标是设计单位(缩放由 `metrics` 携带),这里统一乘上
/// 缩放落到物理像素;面板布局的输出本身就是物理像素,不再变换。
fn paint(
    dc: &ID2D1DeviceContext,
    factory: &ID2D1Factory1,
    f: &Frame,
    text: &Text,
    target: &ID2D1Bitmap1,
    images: &dyn SceneImages,
) {
    unsafe {
        let s = f.metrics.scale();
        let width = panel::panel_width(f.spec, &f.metrics, text);
        let height = panel::panel_height(f.spec, &f.metrics, text);
        let brush: ID2D1SolidColorBrush = dc
            .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
            .expect("建纯色画刷失败");
        dc.Clear(None);
        // 带特效的场景内容先录进命令表:特效图必须在无图层状态下录制,
        // 录完再回到后缓冲接着画普通内容。
        let baked = bake_effects(dc, f, text, target, images);
        let far = D2D_RECT_F {
            left: -100000.0,
            top: -100000.0,
            right: 100000.0,
            bottom: 100000.0,
        };
        if f.opacity < 1.0 {
            dc.PushLayer(
                &layer_params(&far, f.opacity.clamp(0.0, 1.0)),
                None::<&ID2D1Layer>,
            );
        }
        // 面板圆角是全局蒙版:预编辑条与列表的方角内容都收进圆角里。
        let body = panel::Rect::from_xywh(0.5 * s, 0.5 * s, width - s, height - s);
        let mask = factory
            .CreateRoundedRectangleGeometry(&rrect(body, 8.0 * s))
            .ok();
        let mut mask_params = layer_params(&d2d_rect(body), 1.0);
        if let Some(geometry) = mask.as_ref() {
            mask_params.geometricMask = std::mem::ManuallyDrop::new(Some(geometry.cast().unwrap()));
        }
        dc.PushLayer(&mask_params, None::<&ID2D1Layer>);
        // 底色圆角矩形 + 1px 描边(内缩 0.5px 让描边完整落在窗口里)。
        brush.SetColor(&color(f.palette.bg));
        dc.FillRoundedRectangle(&rrect(body, 8.0 * s), &brush);
        brush.SetColor(&color(f.palette.border));
        dc.DrawRoundedRectangle(
            &rrect(
                panel::Rect::from_xywh(
                    body.left + 0.5 * s,
                    body.top + 0.5 * s,
                    (width - 2.0 * s).max(0.0),
                    (height - 2.0 * s).max(0.0),
                ),
                (8.0 - 0.5) * s,
            ),
            &brush,
            s,
            None::<&ID2D1StrokeStyle>,
        );
        // 场景层:文字下面的先画(含特效图)。
        paint_scene(dc, f, text, SceneLayer::BelowText, &baked, images);
        preedit_bar(dc, f, text, &brush, width);
        draw_items(dc, f, text, &brush, width, height);
        paint_scene(dc, f, text, SceneLayer::AboveText, &baked, images);
        if let Some((mx, my)) = f.menu {
            draw_menu(dc, f, text, &brush, mx, my, &baked);
        }
        dc.PopLayer();
        if f.opacity < 1.0 {
            dc.PopLayer();
        }
    }
}

fn layer_params(bounds: &D2D_RECT_F, opacity: f32) -> D2D1_LAYER_PARAMETERS1 {
    D2D1_LAYER_PARAMETERS1 {
        contentBounds: *bounds,
        geometricMask: std::mem::ManuallyDrop::new(None),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: Matrix3x2::identity(),
        opacity,
        opacityBrush: std::mem::ManuallyDrop::new(None),
        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
    }
}

/// 把带特效的场景内容录进命令表并接上 D2D 内置特效图。
/// 录制期间目标在命令表与后缓冲之间来回切,必须发生在推图层之前。
fn bake_effects(
    dc: &ID2D1DeviceContext,
    f: &Frame,
    text: &Text,
    target: &ID2D1Bitmap1,
    images: &dyn SceneImages,
) -> Baked {
    unsafe {
        let mut baked = Baked::empty();
        let s = f.metrics.scale();
        for item in f.scene {
            let Some(effect) = item.effect.as_ref() else {
                baked.blur.push(None);
                baked.shadow.push(None);
                continue;
            };
            baked.blur.push(None);
            baked.shadow.push(None);
            let Ok(cmdlist) = dc.CreateCommandList() else {
                continue;
            };
            let Ok(image) = cmdlist.cast::<ID2D1Image>() else {
                continue;
            };
            let Ok(brush) = dc.CreateSolidColorBrush(&D2D1_COLOR_F::default(), None) else {
                continue;
            };
            dc.SetTransform(&Matrix3x2::scale(s, s));
            dc.SetTarget(&image);
            if let Err(e) = scene_item(dc, f, text, item, &brush, images) {
                println!("场景图元录制约失败:{e}");
            }
            cmdlist.Close().ok();
            dc.SetTarget(target);
            dc.SetTransform(&Matrix3x2::identity());
            let image = &image;
            let effect = match effect {
                EffectOut::Blur { radius } => {
                    dc.CreateEffect(&CLSID_D2D1GaussianBlur)
                        .ok()
                        .map(|effect: ID2D1Effect| {
                            effect.SetInput(0, image, true);
                            let dev = (radius.max(0.0) * s).to_ne_bytes();
                            let _ = effect.SetValue(
                                D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32,
                                D2D1_PROPERTY_TYPE_FLOAT,
                                &dev,
                            );
                            effect
                        })
                }
                EffectOut::Shadow { color, .. } => {
                    dc.CreateEffect(&CLSID_D2D1Shadow)
                        .ok()
                        .map(|effect: ID2D1Effect| {
                            effect.SetInput(0, image, true);
                            let rgba = color.map_or(0x000000, |rgb| scene_fill(rgb, f.palette));
                            // D2D1_SHADOW_PROP_COLOR 是 float4:R、G、B、A 依次打包。
                            let bytes = [
                                ((rgba >> 16) & 0xff) as f32 / 255.0,
                                ((rgba >> 8) & 0xff) as f32 / 255.0,
                                (rgba & 0xff) as f32 / 255.0,
                                0.28,
                            ]
                            .iter()
                            .flat_map(|value| value.to_ne_bytes())
                            .collect::<Vec<u8>>();
                            let _ = effect.SetValue(
                                D2D1_SHADOW_PROP_COLOR.0 as u32,
                                D2D1_PROPERTY_TYPE_FLOAT,
                                &bytes,
                            );
                            let dev = (3.0 * s).to_ne_bytes();
                            let _ = effect.SetValue(
                                D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32,
                                D2D1_PROPERTY_TYPE_FLOAT,
                                &dev,
                            );
                            effect
                        })
                }
            };
            match effect {
                Some(effect) => match item.effect.as_ref() {
                    Some(EffectOut::Blur { .. }) => {
                        baked.blur.last_mut().unwrap().replace(effect);
                    }
                    Some(EffectOut::Shadow { .. }) => {
                        baked.shadow.last_mut().unwrap().replace(effect);
                    }
                    _ => {}
                },
                None => println!("场景特效图建不起来,这一帧走素图"),
            }
        }
        // 菜单投影:把菜单底录进命令表,套阴影特效。
        if f.menu.is_some() {
            let s = f.metrics.scale();
            let (w, h) = (116.0 * s, panel::menu_height(&f.metrics));
            let Ok(cmdlist) = dc.CreateCommandList() else {
                return baked;
            };
            let Ok(image) = cmdlist.cast::<ID2D1Image>() else {
                return baked;
            };
            let Ok(brush) = dc.CreateSolidColorBrush(&D2D1_COLOR_F::default(), None) else {
                return baked;
            };
            dc.SetTarget(&image);
            brush.SetColor(&color(0xffffff));
            dc.FillRoundedRectangle(
                &rrect(panel::Rect::from_xywh(0.0, 0.0, w, h), 4.0 * s),
                &brush,
            );
            cmdlist.Close().ok();
            dc.SetTarget(target);
            if let Ok(effect) = dc.CreateEffect(&CLSID_D2D1Shadow) {
                effect.SetInput(0, &image, true);
                let black = [0.0f32, 0.0, 0.0, 0.11]
                    .iter()
                    .flat_map(|value| value.to_ne_bytes())
                    .collect::<Vec<u8>>();
                let _ = effect.SetValue(
                    D2D1_SHADOW_PROP_COLOR.0 as u32,
                    D2D1_PROPERTY_TYPE_FLOAT,
                    &black,
                );
                let dev = (6.0 * s).to_ne_bytes();
                let _ = effect.SetValue(
                    D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32,
                    D2D1_PROPERTY_TYPE_FLOAT,
                    &dev,
                );
                baked.menu = Some(effect);
            }
        }
        baked
    }
}

/// 画一个层的场景图元;运行时输出按层排好,这里按层筛。
fn paint_scene(
    dc: &ID2D1DeviceContext,
    f: &Frame,
    text: &Text,
    layer: SceneLayer,
    baked: &Baked,
    images: &dyn SceneImages,
) {
    unsafe {
        let s = f.metrics.scale();
        // 特效图在物理像素里;普通图元在设计单位里,乘缩放画。
        dc.SetTransform(&Matrix3x2::scale(s, s));
        for (index, item) in f.scene.iter().enumerate() {
            if item.layer != layer {
                continue;
            }
            match &item.effect {
                Some(EffectOut::Blur { .. }) => {
                    // 模糊图替换本体;画之前回到物理像素坐标。
                    if let Some(effect) = &baked.blur[index] {
                        if let Ok(output) = effect.GetOutput() {
                            dc.SetTransform(&Matrix3x2::identity());
                            let _ = dc.DrawImage(
                                &output,
                                None,
                                None,
                                D2D1_INTERPOLATION_MODE_LINEAR,
                                D2D1_COMPOSITE_MODE_SOURCE_OVER,
                            );
                            dc.SetTransform(&Matrix3x2::scale(s, s));
                        }
                    }
                }
                Some(EffectOut::Shadow { offset, .. }) => {
                    // 阴影垫底,本体照常画在上面。
                    if let Some(effect) = &baked.shadow[index] {
                        if let Ok(output) = effect.GetOutput() {
                            dc.SetTransform(&Matrix3x2::identity());
                            let shift = Vector2 {
                                X: offset[0] * s,
                                Y: offset[1] * s,
                            };
                            let _ = dc.DrawImage(
                                &output,
                                Some(&shift as *const Vector2),
                                None,
                                D2D1_INTERPOLATION_MODE_LINEAR,
                                D2D1_COMPOSITE_MODE_SOURCE_OVER,
                            );
                            dc.SetTransform(&Matrix3x2::scale(s, s));
                        }
                    }
                    let Ok(brush) = dc.CreateSolidColorBrush(&D2D1_COLOR_F::default(), None) else {
                        continue;
                    };
                    let _ = scene_item(dc, f, text, item, &brush, images);
                }
                None => {
                    let Ok(brush) = dc.CreateSolidColorBrush(&D2D1_COLOR_F::default(), None) else {
                        continue;
                    };
                    let _ = scene_item(dc, f, text, item, &brush, images);
                }
            }
        }
        dc.SetTransform(&Matrix3x2::identity());
    }
}

/// 画一个场景图元的内容;调用方已设好设计单位的变换与画刷颜色。
/// 坐标系:rect 是设计单位,图元画在 rect 里或以 rect 左上角为原点。
fn scene_item(
    dc: &ID2D1DeviceContext,
    f: &Frame,
    text: &Text,
    item: &SceneItem,
    brush: &ID2D1SolidColorBrush,
    images: &dyn SceneImages,
) -> Result<(), String> {
    unsafe {
        let rect = panel::Rect::from_xywh(item.rect[0], item.rect[1], item.rect[2], item.rect[3]);
        match &item.body {
            Body::Shape {
                shape,
                radius,
                stroke,
                fill,
            } => {
                brush.SetColor(&color(scene_fill(*fill, f.palette)));
                draw_shape(dc, shape, *radius, *stroke, rect, brush)
            }
            Body::Image { src } => {
                let Some(bitmap) = images.bitmap(f.scene_dir, src) else {
                    return Ok(());
                };
                let _ = dc.DrawBitmap(
                    &bitmap,
                    Some(&d2d_rect(rect) as *const D2D_RECT_F),
                    item.opacity,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    None,
                    None,
                );
                Ok(())
            }
            Body::Sprite { src, frame, grid } => {
                let Some(bitmap) = images.bitmap(f.scene_dir, src) else {
                    return Ok(());
                };
                // 图集横向排帧;@2x/@3x 素材按文件名比例换算源矩形。
                let scale = asset_scale(src);
                let frame_w = grid[0] as f32 * scale;
                let frame_h = grid[1] as f32 * scale;
                let source = D2D_RECT_F {
                    left: *frame as f32 * frame_w,
                    top: 0.0,
                    right: (*frame as f32 + 1.0) * frame_w,
                    bottom: frame_h,
                };
                let _ = dc.DrawBitmap(
                    &bitmap,
                    Some(&d2d_rect(rect) as *const D2D_RECT_F),
                    item.opacity,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    Some(&source as *const D2D_RECT_F),
                    None,
                );
                Ok(())
            }
            Body::Text {
                content,
                font_size,
                fill,
            } => {
                brush.SetColor(&color(scene_fill(*fill, f.palette)));
                draw_text(
                    dc,
                    text,
                    content,
                    *font_size,
                    &panel::Rect::from_xywh(
                        rect.left,
                        rect.top,
                        rect.width().max(1.0),
                        rect.height(),
                    ),
                    brush,
                );
                Ok(())
            }
            Body::Particles {
                shape,
                fill,
                particles,
            } => {
                brush.SetColor(&color(scene_fill(*fill, f.palette)));
                for particle in particles {
                    brush.SetColor(&D2D1_COLOR_F {
                        a: particle.opacity.clamp(0.0, 1.0) * item.opacity,
                        ..color(scene_fill(*fill, f.palette))
                    });
                    let center = Vector2 {
                        X: rect.left + particle.x,
                        Y: rect.top + particle.y,
                    };
                    let half = particle.size.max(0.0) * 0.5;
                    match shape {
                        Shape::Ellipse => dc.FillEllipse(
                            &D2D1_ELLIPSE {
                                point: center,
                                radiusX: half,
                                radiusY: half,
                            },
                            brush,
                        ),
                        _ => dc.FillRectangle(
                            &D2D_RECT_F {
                                left: center.X - half,
                                top: center.Y - half,
                                right: center.X + half,
                                bottom: center.Y + half,
                            },
                            brush,
                        ),
                    }
                }
                Ok(())
            }
        }
    }
}

/// 画一个形状;`stroke` 大于 0 画描边,否则整块填充。
fn draw_shape(
    dc: &ID2D1DeviceContext,
    shape: &Shape,
    radius: f32,
    stroke: f32,
    rect: panel::Rect,
    brush: &ID2D1SolidColorBrush,
) -> Result<(), String> {
    unsafe {
        match shape {
            Shape::Rect => {
                if stroke > 0.0 {
                    dc.DrawRectangle(&d2d_rect(rect), brush, stroke, None::<&ID2D1StrokeStyle>);
                } else {
                    dc.FillRectangle(&d2d_rect(rect), brush);
                }
            }
            Shape::RRect => {
                let rounded = rrect(rect, radius.max(0.0));
                if stroke > 0.0 {
                    dc.DrawRoundedRectangle(&rounded, brush, stroke, None::<&ID2D1StrokeStyle>);
                } else {
                    dc.FillRoundedRectangle(&rounded, brush);
                }
            }
            Shape::Ellipse => {
                let ellipse = D2D1_ELLIPSE {
                    point: Vector2 {
                        X: (rect.left + rect.right) * 0.5,
                        Y: (rect.top + rect.bottom) * 0.5,
                    },
                    radiusX: rect.width() * 0.5,
                    radiusY: rect.height() * 0.5,
                };
                if stroke > 0.0 {
                    dc.DrawEllipse(&ellipse, brush, stroke, None::<&ID2D1StrokeStyle>);
                } else {
                    dc.FillEllipse(&ellipse, brush);
                }
            }
            Shape::Line { to } => {
                let width = if stroke > 0.0 { stroke } else { 1.0 };
                dc.DrawLine(
                    Vector2 {
                        X: rect.left,
                        Y: rect.top,
                    },
                    Vector2 {
                        X: rect.left + to[0],
                        Y: rect.top + to[1],
                    },
                    brush,
                    width,
                    None::<&ID2D1StrokeStyle>,
                );
            }
            // 路径图元这一版画成整块填充兜底;矢量路径等官方场景包需要时再接。
            Shape::Path(_) => {
                dc.FillRectangle(&d2d_rect(rect), brush);
            }
        }
        Ok(())
    }
}

/// 画一段文字;字号随调用方的变换走。段落垂直居中在 rect 里。
fn draw_text(
    dc: &ID2D1DeviceContext,
    text: &Text,
    content: &str,
    size: f32,
    rect: &panel::Rect,
    brush: &ID2D1SolidColorBrush,
) {
    unsafe {
        let Ok(format) = text.format(size) else {
            return;
        };
        let utf16: Vec<u16> = content.encode_utf16().collect();
        dc.DrawText(
            &utf16,
            &format,
            &d2d_rect(*rect),
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

/// 预编辑条:底色、分隔线、拼音与页码翻页箭头。
fn preedit_bar(
    dc: &ID2D1DeviceContext,
    f: &Frame,
    text: &Text,
    brush: &ID2D1SolidColorBrush,
    width: f32,
) {
    unsafe {
        let s = f.metrics.scale();
        brush.SetColor(&color(f.palette.surface_alt));
        dc.FillRectangle(
            &D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: width,
                bottom: panel::PREEDIT_H * s,
            },
            brush,
        );
        // 只有当前页有候选才画分隔线,空页不留一条孤线。
        if !f.spec.items.is_empty() {
            brush.SetColor(&color(f.palette.border));
            dc.FillRectangle(
                &D2D_RECT_F {
                    left: 0.0,
                    top: (panel::PREEDIT_H - 1.0) * s,
                    right: width,
                    bottom: panel::PREEDIT_H * s,
                },
                brush,
            );
        }
        // 翻页箭头:悬停底色是几何,没有字形也要画(悬停反馈看得见)。
        let has_pages = !f.spec.preedit.page_label.is_empty();
        let (prev, label, next) = panel::pager_rects(width, &f.metrics);
        if has_pages {
            for (rect, hit) in [(prev, Hit::PrevPage), (next, Hit::NextPage)] {
                if f.pager == Some(hit) {
                    brush.SetColor(&color(f.palette.border));
                    dc.FillRoundedRectangle(&rrect(rect, 6.0 * s), brush);
                }
            }
        }
        // 拼音行:13px,左侧 12 内边距。
        brush.SetColor(&color(f.palette.text_secondary));
        draw_text(
            dc,
            text,
            &f.spec.preedit.text,
            13.0 * s,
            &panel::Rect::from_xywh(
                12.0 * s,
                0.0,
                (width - 100.0 * s).max(1.0),
                panel::PREEDIT_H * s,
            ),
            brush,
        );
        // 页码与翻页箭头;单页时都没有。
        if !has_pages {
            return;
        }
        brush.SetColor(&color(f.palette.text_secondary));
        for glyph in ["‹", "›"] {
            let rect = if glyph == "‹" { prev } else { next };
            // 字形在箭头区里居中;量宽和绘制同一份文字引擎。
            let glyph_width = text.width(glyph, 15.0 * s);
            let centered = panel::Rect::from_xywh(
                rect.left + (rect.width() - glyph_width) * 0.5,
                rect.top,
                glyph_width.max(1.0),
                rect.height(),
            );
            draw_text(dc, text, glyph, 15.0 * s, &centered, brush);
        }
        brush.SetColor(&color(f.palette.text_tertiary));
        let label_width = text.width(&f.spec.preedit.page_label, 11.0 * s);
        let centered = panel::Rect::from_xywh(
            label.left + (label.width() - label_width) * 0.5,
            label.top,
            label_width.max(1.0),
            label.height(),
        );
        draw_text(
            dc,
            text,
            &f.spec.preedit.page_label,
            11.0 * s,
            &centered,
            brush,
        );
    }
}

/// 候选列表:高亮块、悬停底色、序号、候选文字与注音。
fn draw_items(
    dc: &ID2D1DeviceContext,
    f: &Frame,
    text: &Text,
    brush: &ID2D1SolidColorBrush,
    width: f32,
    height: f32,
) {
    unsafe {
        let s = f.metrics.scale();
        // 翻页滑动会把候选推出面板;裁到描边内侧。
        let _ = dc.PushAxisAlignedClip(
            &D2D_RECT_F {
                left: s,
                top: s,
                right: width - s,
                bottom: height - s,
            },
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        // 滑动/淡入的透明度只作用于列表层,背景与拼音条保持稳定。
        if f.list.opacity < 1.0 {
            let params = layer_params(
                &D2D_RECT_F {
                    left: 0.0,
                    top: panel::PREEDIT_H * s,
                    right: width,
                    bottom: height,
                },
                f.list.opacity.clamp(0.0, 1.0),
            );
            dc.PushLayer(&params, None::<&ID2D1Layer>);
        }
        dc.SetTransform(&Matrix3x2::translation(f.list.dx, 0.0));
        let rects = panel::item_rects(f.spec, &f.metrics, text);
        for (index, (item, rect)) in f.spec.items.iter().zip(&rects).enumerate() {
            let (x, y) = (rect.left, rect.top);
            let row_h = rect.height();
            // 选中块:柔和底色 + 左侧 2px 主色条;悬停块只给非选中行。
            if item.selected {
                brush.SetColor(&color(f.palette.selected_bg));
                dc.FillRoundedRectangle(&rrect(*rect, 4.0 * s), brush);
                brush.SetColor(&color(f.palette.accent_text));
                dc.FillRoundedRectangle(
                    &rrect(
                        panel::Rect::from_xywh(
                            x + 8.0 * s,
                            y + (row_h - 14.0 * s) * 0.5,
                            2.0 * s,
                            14.0 * s,
                        ),
                        s,
                    ),
                    brush,
                );
            } else if f.hover == Some(index) {
                brush.SetColor(&color(f.palette.hover));
                dc.FillRoundedRectangle(&rrect(*rect, 4.0 * s), brush);
            }
            let index_rgb = if item.selected {
                f.palette.accent_text
            } else {
                f.palette.text_tertiary
            };
            let text_rgb = if item.selected {
                f.palette.selected_text
            } else {
                f.palette.text
            };
            // 序号
            brush.SetColor(&color(index_rgb));
            draw_text(
                dc,
                text,
                &format!("{}", index + 1),
                f.metrics.small,
                &panel::Rect::from_xywh(x + (8.0 + 2.0 + 4.0) * s, y, 20.0 * s, row_h),
                brush,
            );
            brush.SetColor(&color(text_rgb));
            // 候选文字;超宽部分按 max_text_width 裁掉(DrawText 不换行也不裁,夹取在这里)。
            let text_x = x + (8.0 + 2.0 + 4.0 + 12.0 + 4.0) * s;
            let _ = dc.PushAxisAlignedClip(
                &D2D_RECT_F {
                    left: text_x,
                    top: y,
                    right: text_x + f.spec.max_text_width.max(1.0),
                    bottom: y + row_h,
                },
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            draw_text(
                dc,
                text,
                &item.text,
                f.metrics.text,
                &panel::Rect::from_xywh(text_x, y, f.spec.max_text_width.max(1.0), row_h),
                brush,
            );
            dc.PopAxisAlignedClip();
            // 注音/来源小字,跟在候选文字后面。
            if !item.hint.is_empty() {
                let text_width = text.width(&item.text, f.metrics.text);
                draw_text(
                    dc,
                    text,
                    &item.hint,
                    f.metrics.small,
                    &panel::Rect::from_xywh(text_x + text_width + 4.0 * s, y, 100.0 * s, row_h),
                    brush,
                );
            }
        }
        dc.SetTransform(&Matrix3x2::identity());
        if f.list.opacity < 1.0 {
            dc.PopLayer();
        }
        dc.PopAxisAlignedClip();
    }
}

/// 画右键菜单;`(x, y)` 是菜单左上角,`hover` 是悬停的菜单项,尺寸随缩放。
fn draw_menu(
    dc: &ID2D1DeviceContext,
    f: &Frame,
    text: &Text,
    brush: &ID2D1SolidColorBrush,
    x: f32,
    y: f32,
    baked: &Baked,
) {
    unsafe {
        let s = f.metrics.scale();
        let w = 116.0 * s;
        let h = panel::menu_height(&f.metrics);
        // 轻投影把菜单从候选层上托起来;投影图已在无图层状态录好。
        if let Some(effect) = &baked.menu {
            if let Ok(output) = effect.GetOutput() {
                let _ = dc.DrawImage(
                    &output,
                    Some(&Vector2 {
                        X: x + s,
                        Y: y + 2.0 * s,
                    } as *const Vector2),
                    None,
                    D2D1_INTERPOLATION_MODE_LINEAR,
                    D2D1_COMPOSITE_MODE_SOURCE_OVER,
                );
            }
        }
        let body = panel::Rect::from_xywh(x + 0.5 * s, y + 0.5 * s, w - s, h - s);
        brush.SetColor(&color(f.palette.bg));
        dc.FillRoundedRectangle(&rrect(body, 4.0 * s), brush);
        brush.SetColor(&color(f.palette.border));
        dc.DrawRoundedRectangle(
            &rrect(
                panel::Rect::from_xywh(
                    body.left + 0.5 * s,
                    body.top + 0.5 * s,
                    body.width() - s,
                    body.height() - s,
                ),
                3.5 * s,
            ),
            brush,
            s,
            None::<&ID2D1StrokeStyle>,
        );
        // 悬停行底色先画:没有字形也要留住反馈。
        if let Some(index) = f.menu_hover {
            brush.SetColor(&color(f.palette.hover));
            dc.FillRoundedRectangle(
                &rrect(
                    panel::Rect::from_xywh(
                        x + 4.0 * s,
                        y + (4.0 + index as f32 * 28.0) * s,
                        (w - 8.0 * s).max(0.0),
                        26.0 * s,
                    ),
                    4.0 * s,
                ),
                brush,
            );
        }
        for (index, label) in panel::MENU_ITEMS.iter().enumerate() {
            brush.SetColor(&color(if f.menu_hover == Some(index) {
                f.palette.text
            } else {
                f.palette.text_secondary
            }));
            draw_text(
                dc,
                text,
                label,
                12.0 * s,
                &panel::Rect::from_xywh(
                    x + 8.0 * s,
                    y + (4.0 + index as f32 * 28.0) * s,
                    w - 16.0 * s,
                    26.0 * s,
                ),
                brush,
            );
        }
    }
}
