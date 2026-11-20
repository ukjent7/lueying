//! 候选窗的 D2D 管线:D3D11 设备 + Direct2D 设备上下文 + DirectComposition 合成交换链。
//!
//! 表面、出帧与提交在这里;窗口、动效与命中在 `ui::d2d`。交换链交给 DComp
//! 合成,圆角与淡入淡出靠逐像素 alpha(预乘)。没有硬件适配器时退到 WARP
//! (软件 D3D11,D2D 官方支持),同一套代码路径,画面一致;设备被系统摘掉
//! 就地整条重建,窗口不用换。离屏路径(`Offscreen`)不依赖窗口与 DComp,
//! 给没有桌面会话的环境留一条能验证出帧与像素的路。

use std::cell::RefCell;
use std::collections::HashMap;

use lueying_render::paint::Point as PaintPoint;
use lueying_render::panel::{self, ListFx, Measure, Metrics, Palette, PanelSpec};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F, D2D1_GRADIENT_STOP};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_CAP_STYLE_ROUND,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_EXTEND_MODE_WRAP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
    D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES1, D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device,
    ID2D1DeviceContext, ID2D1Factory1, ID2D1Image, ID2D1Layer, ID2D1RenderTarget,
    ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Texture2D,
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
use windows::core::{Interface, w};
use windows_numerics::{Matrix3x2, Vector2};

/// 交换链缓冲数;双缓冲配 flip 模型。
const BUFFER_COUNT: usize = 2;

/// 展开/收起的形变:同一块玻璃从旧形态长到新形态,内容交叉淡入。
/// 玻璃只画一层,旧内容与新内容都压在同一块材质上,不玻璃叠玻璃。
pub(super) struct Morph<'a> {
    /// 另一态(展开时是固定条,收起时是棋盘)。
    pub from: &'a PanelSpec,
    /// 弹簧进度 0..1。
    pub progress: f32,
    /// 窗口当前高度(物理像素);与窗口矩形逐帧一致,避免底缘露出一像素缝。
    pub height: f32,
    /// 候选窗在光标上方,向上生长;旧内容贴底,新内容贴顶。
    pub up: bool,
    /// 目标态是展开:箭头由 ⌄ 转到 ⌃;收起时反着转。
    pub expanding: bool,
}

/// 一帧的绘制输入;窗口层换算,管线照画。
pub(super) struct Frame<'a> {
    pub spec: &'a PanelSpec,
    pub palette: &'a Palette,
    pub metrics: Metrics,
    /// 背板由 DWM 亚克力出底色;自绘只铺半透明内容层。
    pub glass: bool,
    /// 自捕获光学背板(E 路线)与绘制偏移;None 时玻璃回退 DWM 亚克力。
    pub backdrop: Option<(&'a super::glass::GlassImages, (f32, f32))>,
    /// 整窗透明度(淡入/淡出)。
    pub opacity: f32,
    /// 候选列表动效(翻页滑动)。
    pub list: ListFx,
    /// 页内悬停候选。
    pub hover: Option<usize>,
    /// 悬停的条尾 ⌄ 按钮。
    pub chev: bool,
    /// 鼠标停在棋盘滚动条上;显示加宽态。
    pub scroll_hover: bool,
    /// 展开/收起的形变;静止时为 None。
    pub morph: Option<Morph<'a>>,
    /// 右键菜单左上角。
    pub menu: Option<(f32, f32)>,
    /// 悬停的菜单项。
    pub menu_hover: Option<usize>,
}

/// rim 的双层配色:外沿暖黑发丝 + 内侧暖白镜面。
#[derive(Clone, Copy)]
struct RimStyle {
    edge: [f32; 3],
    edge_alpha: f32,
    inner: [f32; 3],
    base: f32,
    peak: f32,
}

impl RimStyle {
    /// 当前玻璃候选条:外暖黑 α0.85 + 内暖白 1px 发丝。
    const GLASS: Self = Self {
        edge: [0.05, 0.039, 0.027],
        edge_alpha: 0.85,
        inner: [1.0, 0.949, 0.863],
        base: 0x14 as f32,
        peak: 0x2e as f32,
    };
}

/// 出帧路径的最小接口:画一帧、读回一个像素。
/// 窗口路径与离屏路径共用,自检对两条路径用同一套像素期望。
pub(super) trait Target {
    fn paint_frame(&mut self, f: &Frame, text: &Text) -> Result<(), String>;
    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)>;
}

/// 文字引擎:DirectWrite 工厂 + 按字号缓存的文字格式;布局与绘制共用,
/// 量出来的窗才装得下画出来的字。
pub(crate) struct Text {
    dwrite: IDWriteFactory,
    formats: RefCell<HashMap<u32, IDWriteTextFormat>>,
    /// 各字号的墨迹中心偏移(见 `ink_dy`)。
    inks: RefCell<HashMap<u32, f32>>,
}

impl Text {
    pub(super) fn new() -> Result<Self, String> {
        Ok(Self {
            dwrite: unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) }
                .map_err(|e| format!("DWriteCreateFactory: {e}"))?,
            formats: RefCell::new(HashMap::new()),
            inks: RefCell::new(HashMap::new()),
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

    /// 墨迹中心相对行盒中心的竖直偏移(物理像素)。
    ///
    /// 段落居中把行盒放到矩形中线,但字形可见墨迹在行盒里偏上;用
    /// DirectWrite 的过盈量实测「字」的墨迹上下界,绘制前回正,
    /// 候选、序号、注音共享同一行盒基准,基线仍按字体原生比例对齐。
    fn ink_dy(&self, size: f32) -> f32 {
        let key = size.to_bits();
        if let Some(dy) = self.inks.borrow().get(&key) {
            return *dy;
        }
        let mut dy = 0.0;
        if let Ok(format) = self.format(size) {
            unsafe {
                let probe: Vec<u16> = "字".encode_utf16().collect();
                if let Ok(layout) = self.dwrite.CreateTextLayout(&probe, &format, 1000.0, 200.0)
                    && let Ok(overhang) = layout.GetOverhangMetrics()
                {
                    dy = 0.5 * (overhang.bottom - overhang.top);
                }
            }
        }
        self.inks.borrow_mut().insert(key, dy);
        dy
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
    /// 自建帧纹理与 D2D 位图;D2D 画它,Present 前拷进后缓冲。
    tex: Option<ID3D11Texture2D>,
    bmp: Option<ID2D1Bitmap1>,
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
                tex: None,
                bmp: None,
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

    /// 设备上下文;背板捕获与效果链在出帧之外上传位图用。
    pub(super) fn context(&self) -> ID2D1DeviceContext {
        self.core.dc.clone()
    }

    /// 取证(temp/lueying-dump-on):读回帧纹理存 BMP。实机截图因排除捕获
    /// 拿不到候选窗,数值取证走这个开关。
    pub(super) fn dump_frame(&self, path: &std::path::Path) {
        let Some(tex) = self.tex.as_ref() else {
            return;
        };
        let (w, h) = self.size;
        if let Some(bgra) = read_texture_bgra(&self.core.d3d, &self.core.context, tex, w, h) {
            let _ = super::selftest::save_bmp(path, &bgra, w, h);
        }
    }

    /// 窗口尺寸变化时改交换链与自建帧纹理。
    /// D2D 不直接画交换链表面(实机该链路出过冻帧/空帧):画进自建纹理
    /// (与离屏自检同一条被验证的路),Present 前 D3D CopyResource 进后缓冲。
    pub(super) fn rebuild_surfaces(&mut self, w: u32, h: u32) -> Result<(), String> {
        if self.size == (w, h) {
            return Ok(());
        }
        unsafe {
            self.swapchain
                .ResizeBuffers(0, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))
                .map_err(|e| format!("ResizeBuffers({w}x{h}): {e}"))?;
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
                ..Default::default()
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            self.core
                .d3d
                .CreateTexture2D(&desc, None, Some(&mut tex))
                .map_err(|e| format!("帧纹理: {e}"))?;
            let tex = tex.ok_or("帧纹理没返回")?;
            let surface: windows::Win32::Graphics::Dxgi::IDXGISurface =
                tex.cast().map_err(|e| format!("帧纹理表面: {e}"))?;
            let bmp = self
                .core
                .dc
                .CreateBitmapFromDxgiSurface(&surface, None)
                .map_err(|e| format!("帧位图: {e}"))?;
            self.tex = Some(tex);
            self.bmp = Some(bmp);
        }
        self.size = (w, h);
        Ok(())
    }

    pub(super) fn present(&mut self) -> Result<(), String> {
        unsafe {
            // 自建帧纹理拷进当帧后缓冲,再提交合成。
            let index = self.swapchain.GetCurrentBackBufferIndex();
            let buffer: ID3D11Texture2D = self
                .swapchain
                .GetBuffer(index)
                .map_err(|e| format!("GetBuffer({index}): {e}"))?;
            let tex = self.tex.as_ref().ok_or("帧纹理未建")?;
            self.core.context.CopyResource(&buffer, tex);
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

impl Target for Pipeline {
    fn paint_frame(&mut self, f: &Frame, text: &Text) -> Result<(), String> {
        let bitmap = self.bmp.as_ref().ok_or("帧位图未建")?.clone();
        let dc = self.core.dc.clone();
        let factory = self.core.d2d.clone();
        unsafe {
            dc.SetTarget(&bitmap);
            dc.BeginDraw();
            paint(&dc, &factory, f, text, true);
            let result = dc.EndDraw(None, None);
            dc.SetTarget(None);
            result
        }
        .map_err(|e| format!("EndDraw: {e}"))
    }

    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        let tex = self.tex.as_ref()?;
        read_bgr(&self.core.d3d, &self.core.context, tex, x, y)
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

impl Target for Offscreen {
    fn paint_frame(&mut self, f: &Frame, text: &Text) -> Result<(), String> {
        let dc = self.core.dc.clone();
        let factory = self.core.d2d.clone();
        let bitmap = self.bitmap.clone();
        unsafe {
            dc.SetTarget(&bitmap);
            dc.BeginDraw();
            paint(&dc, &factory, f, text, true);
            dc.EndDraw(None, None)
        }
        .map_err(|e| format!("EndDraw: {e}"))
    }

    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        read_bgr(&self.core.d3d, &self.core.context, &self.texture, x, y)
    }
}

impl Offscreen {
    /// 设备上下文;预览合成背景与上传背板用。
    pub(super) fn context(&self) -> ID2D1DeviceContext {
        self.core.dc.clone()
    }

    /// 设好目标画一段自定义内容(预览合成桌面用);闭包外不持目标。
    pub(super) fn draw_with(&self, f: impl FnOnce(&ID2D1DeviceContext)) -> Result<(), String> {
        let dc = self.core.dc.clone();
        let bitmap = self.bitmap.clone();
        unsafe {
            dc.SetTarget(&bitmap);
            dc.BeginDraw();
            f(&dc);
            dc.EndDraw(None, None)
        }
        .map_err(|e| format!("EndDraw: {e}"))
    }

    /// 整块铺一个底色(玻璃预览的合成背景)。
    pub(super) fn fill_all(&mut self, color: D2D1_COLOR_F) -> Result<(), String> {
        let dc = self.core.dc.clone();
        let bitmap = self.bitmap.clone();
        unsafe {
            dc.SetTarget(&bitmap);
            dc.BeginDraw();
            dc.Clear(Some(&color));
            dc.EndDraw(None, None)
        }
        .map_err(|e| format!("Clear: {e}"))
    }

    /// 不清屏直接画一帧:玻璃预览先铺合成背景,再把玻璃层叠上去。
    pub(super) fn paint_over(&mut self, f: &Frame, text: &Text) -> Result<(), String> {
        let dc = self.core.dc.clone();
        let factory = self.core.d2d.clone();
        let bitmap = self.bitmap.clone();
        unsafe {
            dc.SetTarget(&bitmap);
            dc.BeginDraw();
            paint(&dc, &factory, f, text, false);
            dc.EndDraw(None, None)
        }
        .map_err(|e| format!("EndDraw: {e}"))
    }

    /// 一次整块读回纹理(BGRA,自上而下);BMP 预览用,别拿它做逐像素断言。
    pub(super) fn read_bgra(&mut self, w: u32, h: u32) -> Option<Vec<u8>> {
        read_texture_bgra(&self.core.d3d, &self.core.context, &self.texture, w, h)
    }
}

/// 一次整块读回纹理(BGRA,自上而下);预览与实机取证共用。
fn read_texture_bgra(
    d3d: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
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
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            let row = mapped.pData as *const u8;
            let base = y * mapped.RowPitch as usize;
            for x in 0..w as usize {
                let at = base + x * 4;
                out.extend_from_slice(&[
                    *row.add(at),
                    *row.add(at + 1),
                    *row.add(at + 2),
                    *row.add(at + 3),
                ]);
            }
        }
        context.Unmap(&staging, 0);
        Some(out)
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

/// 0xRRGGBB 带透明度转 D2D 颜色;材质背板上的内容层用。
fn color_a(rgb: u32, alpha: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        a: f32::from(alpha) / 255.0,
        ..color(rgb)
    }
}

impl Frame<'_> {
    /// 实底照原样上色;材质背板上压成半透明,免得盖住亚克力。
    fn tint(&self, rgb: u32, alpha: u8) -> D2D1_COLOR_F {
        if self.glass {
            color_a(rgb, alpha)
        } else {
            color(rgb)
        }
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

/// 画一帧完整画面;窗口与离屏两条路径共用,像素结果一致。
///
/// 面板布局的输出本身就是物理像素;窗口摆位在窗口层完成,这里不再变换。
fn paint(dc: &ID2D1DeviceContext, factory: &ID2D1Factory1, f: &Frame, text: &Text, clear: bool) {
    unsafe {
        let s = f.metrics.scale();
        let width = panel::panel_width(f.spec, &f.metrics, text);
        let height = match &f.morph {
            // 形变期间玻璃高度就是窗口高度:两者共用同一个矩形,底缘严丝合缝。
            Some(m) => m.height.max(1.0),
            None => panel::panel_height(f.spec, &f.metrics, text),
        };
        let brush: ID2D1SolidColorBrush = dc
            .CreateSolidColorBrush(&D2D1_COLOR_F::default(), None)
            .expect("建纯色画刷失败");
        if clear {
            dc.Clear(None);
        }
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
            .CreateRoundedRectangleGeometry(&rrect(body, panel::PANEL_RADIUS * s))
            .ok();
        let mut mask_params = layer_params(&d2d_rect(body), 1.0);
        if let Some(geometry) = mask.as_ref() {
            mask_params.geometricMask = std::mem::ManuallyDrop::new(Some(geometry.cast().unwrap()));
        }
        dc.PushLayer(&mask_params, None::<&ID2D1Layer>);
        // E 路线光学背板:身体强模糊铺最底;透镜环(折射+色散+轻模糊)
        // 经 SDF alpha 环从中心向边缘渐入(渐进变模糊,无硬缝);噪点消
        // 色带。其上再铺固定烟熏层与双发丝 rim。
        if f.glass
            && let Some((g, offset)) = f.backdrop
        {
            super::glass::draw_backdrop(dc, &g.body, offset);
            if let Ok(ring_brush) = dc.CreateBitmapBrush(&g.ring, None, None) {
                ring_brush.SetTransform(&Matrix3x2::translation(offset.0, offset.1));
                let mut lp = layer_params(&d2d_rect(body), 1.0);
                lp.opacityBrush = std::mem::ManuallyDrop::new(ring_brush.cast().ok());
                if let Some(geometry) = mask.as_ref() {
                    lp.geometricMask = std::mem::ManuallyDrop::new(Some(geometry.cast().unwrap()));
                }
                dc.PushLayer(&lp, None::<&ID2D1Layer>);
                super::glass::draw_backdrop(dc, &g.lens, offset);
                dc.PopLayer();
            }
            if let Ok(grain_brush) = dc.CreateBitmapBrush(
                &g.grain,
                Some(&D2D1_BITMAP_BRUSH_PROPERTIES1 {
                    extendModeX: D2D1_EXTEND_MODE_WRAP,
                    extendModeY: D2D1_EXTEND_MODE_WRAP,
                    ..Default::default()
                }),
                None,
            ) {
                grain_brush.SetOpacity(0.02);
                dc.FillRoundedRectangle(&rrect(body, panel::PANEL_RADIUS * s), &grain_brush);
            }
        }
        // 底色圆角矩形 + 1px 描边(内缩 0.5px 让描边完整落在窗口里)。
        // 实底跟主题走。
        if !f.glass {
            brush.SetColor(&color(f.palette.bg));
            dc.FillRoundedRectangle(&rrect(body, panel::PANEL_RADIUS * s), &brush);
            brush.SetColor(&color(f.palette.border));
        } else {
            // 候选条的玻璃身份恒定:同一层 α≈0.42 的暖墨烟熏 #4d4436
            // 压在所有背板上,白底得约 180、黑底(30)得约 50。
            // 明暗只由固定透过率随背板走,不再按亮度换极性;
            // 暖墨的色偏把中性灰和 macOS 拉开,背景照样透出来。
            brush.SetColor(&color_a(0x4d4436, 0x6c));
            dc.FillRoundedRectangle(&rrect(body, panel::PANEL_RADIUS * s), &brush);
        }
        // 描边:玻璃走法线镜面 rim——亮度随法线与光向夹角变化。
        // 预乘彩色位图 1:1 直画,像素级精确;opacityBrush 层采样有原点偏移。
        if f.glass {
            if let Some(rim) = rim_mask(dc, width, height, s, RimStyle::GLASS)
                && let Ok(img) = rim.cast::<ID2D1Image>()
            {
                super::glass::draw_backdrop(dc, &img, (0.0, 0.0));
            }
        } else {
            dc.DrawRoundedRectangle(
                &rrect(
                    panel::Rect::from_xywh(
                        body.left + 0.5 * s,
                        body.top + 0.5 * s,
                        (width - 2.0 * s).max(0.0),
                        (height - 2.0 * s).max(0.0),
                    ),
                    (panel::PANEL_RADIUS - 0.5) * s,
                ),
                &brush,
                s,
                None::<&ID2D1StrokeStyle>,
            );
        }
        // 展开/收起:两块内容压在同一层玻璃上交叉淡入。旧内容贴住光标侧
        // (在上方生长时贴底),新内容贴顶;弹簧过半后旧的退场、新的登场。
        if let Some(m) = &f.morph {
            let from_h = panel::panel_height(m.from, &f.metrics, text);
            let dy = if m.up { height - from_h } else { 0.0 };
            let main = smoothstep(0.35, 0.85, m.progress);
            draw_items(
                dc,
                factory,
                f,
                text,
                &brush,
                m.from,
                width,
                height,
                1.0 - main,
                dy,
                panel::ListFx::NEUTRAL,
                None,
            );
            draw_items(
                dc, factory, f, text, &brush, f.spec, width, height, main, 0.0, f.list, f.hover,
            );
        } else {
            draw_items(
                dc, factory, f, text, &brush, f.spec, width, height, 1.0, 0.0, f.list, f.hover,
            );
        }
        draw_control(dc, factory, f, text, &brush, width, height);
        if let Some((mx, my)) = f.menu {
            draw_menu(dc, f, text, &brush, mx, my);
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

/// 交叉淡入的平滑曲线;两端导数为零,内容不会突然出现或停住。
fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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
        // 行盒居中会把墨迹留在偏上的位置,按实测偏移把绘制矩形上移回正。
        let dy = text.ink_dy(size);
        dc.DrawText(
            &utf16,
            &format,
            &d2d_rect(panel::Rect::from_xywh(
                rect.left,
                rect.top - dy,
                rect.width(),
                rect.height(),
            )),
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

/// 玻璃镜面 rim:全分辨率 SDF 预乘位图,逐像素法线×光向(左上光源),
/// 覆盖度抗锯齿。旧版 128 段 DrawLine 圆帽在接缝重叠出周期性亮毛刺、
/// 弧上亮度逐段阶跃;SDF 无接缝无阶跃。
///
/// 双发丝:外沿 1px 轮廓 + 内侧 1px 镜面,颜色由 [`RimStyle`] 给;
/// 玻璃是黑白,陶瓷是暖釉。两层同一张预乘位图,没有接缝。
fn rim_mask(
    dc: &ID2D1DeviceContext,
    w: f32,
    h: f32,
    s: f32,
    style: RimStyle,
) -> Option<ID2D1Bitmap1> {
    let (wi, hi) = (w as u32, h as u32);
    // SDF 与圆角蒙版同几何(内缩 0.5s、半径 10s):d=0 就是面板外沿。
    // 双发丝按物理像素排:外 1px 黑(k 0.5..1.5)、内 1px 白(k 1.5..2.5),
    // 不随 DPI 加宽;旧带按 s 放大,亮背板上就是一条白杠。
    let (l, t, r, b) = (0.5 * s, 0.5 * s, w - 0.5 * s, h - 0.5 * s);
    let rad = (panel::PANEL_RADIUS * s)
        .min((r - l) * 0.5)
        .min((b - t) * 0.5);
    let (cx, cy) = ((l + r) * 0.5, (t + b) * 0.5);
    let (hx, hy) = ((r - l) * 0.5 - rad, (b - t) * 0.5 - rad);
    let mut px = vec![0u8; wi as usize * hi as usize * 4];
    for y in 0..hi {
        let sy = (y as f32 + 0.5 - cy).signum();
        let dy = (y as f32 + 0.5 - cy).abs() - hy;
        for x in 0..wi {
            let sx = (x as f32 + 0.5 - cx).signum();
            let dx = (x as f32 + 0.5 - cx).abs() - hx;
            let (ax, ay) = (dx.max(0.0), dy.max(0.0));
            let d = (ax * ax + ay * ay).sqrt() + dx.max(dy).min(0.0) - rad;
            // k 是距面板外沿向内的物理像素;轮廓与镜面各占 1px,相邻不重叠。
            let k = -d;
            let inner = ((k + 0.5).min(2.5) - (k - 0.5).max(1.5)).clamp(0.0, 1.0);
            let outer =
                ((k + 0.5).min(1.5) - (k - 0.5).max(0.5)).clamp(0.0, 1.0) * style.edge_alpha;
            if inner <= 0.0 && outer <= 0.0 {
                continue;
            }
            let (nx, ny) = if ax > 0.0 || ay > 0.0 {
                let il = 1.0 / (ax * ax + ay * ay).sqrt();
                (ax * il * sx, ay * il * sy)
            } else if dx > dy {
                (sx, 0.0)
            } else {
                (0.0, sy)
            };
            // 镜面项三次方收高光;峰值与 macOS 候选条同量级,顶边不会亮成白杠。
            let dot = nx * -0.5 + ny * -0.866;
            let spec = dot.max(0.0).powi(3);
            let a = (inner * (style.base + (style.peak - style.base) * spec)).min(255.0);
            let ip = (a / 255.0) * (1.0 - outer);
            let i = (y as usize * wi as usize + x as usize) * 4;
            px[i] = ((style.inner[0] * ip + style.edge[0] * outer) * 255.0).min(255.0) as u8;
            px[i + 1] = ((style.inner[1] * ip + style.edge[1] * outer) * 255.0).min(255.0) as u8;
            px[i + 2] = ((style.inner[2] * ip + style.edge[2] * outer) * 255.0).min(255.0) as u8;
            px[i + 3] = ((ip + outer) * 255.0).min(255.0) as u8;
        }
    }
    super::glass::upload(
        dc,
        wi,
        hi,
        &px,
        96.0,
        windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE_PREMULTIPLIED,
    )
}

/// 可滚动的一屏;候选不足一屏时没有滚动条,控制轨只留收起钮。
fn control_scroll(spec: &PanelSpec) -> Option<panel::Scroll> {
    spec.scroll
        .filter(|scroll| scroll.cap > 0 && scroll.len > scroll.cap)
}

/// 控件在当前态的形状:固定条/无滚动时是 22px 圆钮,可滚动时是右缘轨道。
fn control_shape(
    spec: &PanelSpec,
    metrics: &Metrics,
    measure: &dyn Measure,
    width: f32,
    height: f32,
    hover: bool,
) -> Option<panel::Rect> {
    if let Some(scroll) = control_scroll(spec) {
        return panel::scrollbar_rects(scroll, width, height, metrics, hover)
            .map(|(track, _)| track);
    }
    panel::chevron_rect(spec, metrics, measure)
}

/// 箭头所在的矩形:展开为轨道时缩到顶帽里的 14px 图标。
fn control_glyph_rect(
    spec: &PanelSpec,
    metrics: &Metrics,
    measure: &dyn Measure,
) -> Option<panel::Rect> {
    let cap = panel::chevron_rect(spec, metrics, measure)?;
    if control_scroll(spec).is_none() {
        return Some(cap);
    }
    let s = metrics.scale();
    let size = 14.0 * s;
    Some(panel::Rect::from_xywh(
        cap.center_x() - size * 0.5,
        cap.center_y() - size * 0.5,
        size,
        size,
    ))
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp_rect(a: panel::Rect, b: panel::Rect, t: f32) -> panel::Rect {
    panel::Rect::from_ltrb(
        lerp(a.left, b.left, t),
        lerp(a.top, b.top, t),
        lerp(a.right, b.right, t),
        lerp(a.bottom, b.bottom, t),
    )
}

/// 右上控制:圆钮与滚动条是同一个形状。
///
/// 展开且有滚动内容时,22px 圆钮向下拉成右缘轨道,箭头旋转到 ⌃ 并淡成
/// 顶帽;收起时轨道缩回圆钮。滚动条不跟着列表淡入淡出,窗口形变中也不
/// 被内容裁剪层带走。
#[allow(clippy::too_many_arguments)]
fn draw_control(
    dc: &ID2D1DeviceContext,
    factory: &ID2D1Factory1,
    f: &Frame,
    text: &Text,
    brush: &ID2D1SolidColorBrush,
    width: f32,
    height: f32,
) {
    let (Some(target_shape), Some(target_glyph)) = (
        control_shape(f.spec, &f.metrics, text, width, height, f.scroll_hover),
        control_glyph_rect(f.spec, &f.metrics, text),
    ) else {
        return;
    };
    let source = f.morph.as_ref().map(|m| m.from);
    let source_scroll = source.and_then(control_scroll);
    let target_scroll = control_scroll(f.spec);
    let progress = f.morph.as_ref().map_or(1.0, |m| m.progress.clamp(0.0, 1.0));
    let source_shape = source
        .and_then(|spec| control_shape(spec, &f.metrics, text, width, height, f.scroll_hover))
        .unwrap_or(target_shape);
    let source_glyph = source
        .and_then(|spec| control_glyph_rect(spec, &f.metrics, text))
        .unwrap_or(target_glyph);
    let shape = lerp_rect(source_shape, target_shape, progress);
    let glyph = lerp_rect(source_glyph, target_glyph, progress);
    let turn = {
        let turn_of = |spec: &PanelSpec| {
            if spec.expand && matches!(spec.layout, panel::Layout::Wrap) {
                std::f32::consts::PI
            } else {
                0.0
            }
        };
        let from = source.map_or(turn_of(f.spec), turn_of);
        lerp(from, turn_of(f.spec), progress)
    };
    let alpha_of = |spec: &PanelSpec| {
        if control_scroll(spec).is_some() && !f.chev {
            0.55
        } else {
            1.0
        }
    };
    let glyph_alpha = lerp(
        source.map_or(alpha_of(f.spec), alpha_of),
        alpha_of(f.spec),
        progress,
    );
    unsafe {
        let s = f.metrics.scale();
        // 淡竖槽把顶帽与滚动条收进同一件控件;形变时随轨道一起长出来。
        let rail_spec = if target_scroll.is_some() {
            Some(f.spec)
        } else {
            source
        };
        if let Some(rail_spec) = rail_spec
            && let Some(rail) = panel::rail_rect(rail_spec, &f.metrics, text, height)
        {
            let fade = if f.morph.is_some() {
                if target_scroll.is_some() {
                    progress
                } else {
                    1.0 - progress
                }
            } else {
                1.0
            };
            brush.SetColor(&color_a(0x000000, (0x34 as f32 * fade) as u8));
            dc.FillRoundedRectangle(&rrect(rail, rail.width() * 0.5), brush);
            brush.SetColor(&color_a(0xffffff, (0x14 as f32 * fade) as u8));
            dc.DrawRoundedRectangle(
                &rrect(rail, rail.width() * 0.5),
                brush,
                s,
                None::<&ID2D1StrokeStyle>,
            );
        }
        // 只有一个形状:圆钮和轨道共用同一次矩形插值。
        let on_rail = target_scroll.is_some() || source_scroll.is_some();
        let show_shape = on_rail
            || f.morph.is_some()
            || f.chev
            || (f.spec.expand && matches!(f.spec.layout, panel::Layout::Wrap));
        if show_shape {
            let alpha = if on_rail {
                0x40
            } else if f.chev {
                0x66
            } else {
                0x38
            };
            brush.SetColor(&color_a(f.palette.border, alpha));
            let radius = shape.width().min(shape.height()) * 0.5;
            dc.FillRoundedRectangle(&rrect(shape, radius), brush);
        }
        // 滑块:展开后半段淡入,收起前半段先淡出。
        let thumb_alpha = if let Some(m) = &f.morph {
            if m.expanding {
                smoothstep(0.55, 1.0, progress)
            } else {
                1.0 - smoothstep(0.0, 0.45, progress)
            }
        } else if target_scroll.is_some() {
            1.0
        } else {
            0.0
        };
        if thumb_alpha > 0.01
            && let Some(scroll) = target_scroll.or(source_scroll)
            && let Some((_, thumb)) =
                panel::scrollbar_rects(scroll, width, height, &f.metrics, f.scroll_hover)
        {
            let alpha = ((if f.scroll_hover { 0xdd } else { 0xbb }) as f32) * thumb_alpha;
            brush.SetColor(&color_a(f.palette.text_tertiary, alpha as u8));
            dc.FillRoundedRectangle(&rrect(thumb, thumb.width() * 0.5), brush);
        }
        // 悬浮顶帽:轨道上方浮出圆钮,给收起一个明确目标。
        if f.morph.is_none()
            && target_scroll.is_some()
            && f.chev
            && let Some(cap) = panel::chevron_rect(f.spec, &f.metrics, text)
        {
            brush.SetColor(&color_a(f.palette.border, 0x66));
            dc.FillRoundedRectangle(&rrect(cap, cap.height() * 0.5), brush);
            brush.SetColor(&color_a(f.palette.border, 0x2e));
            dc.DrawRoundedRectangle(
                &rrect(cap, cap.height() * 0.5),
                brush,
                s,
                None::<&ID2D1StrokeStyle>,
            );
        }
        // 矢量折线几何居中,不再依赖字体字形墨迹在行盒里的位置。
        let style = factory
            .CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES1 {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    ..Default::default()
                },
                None,
            )
            .ok()
            .and_then(|style| style.cast::<ID2D1StrokeStyle>().ok());
        let [a, b, c] = panel::chevron_points_turn(glyph, turn, s);
        let v = |p: PaintPoint| Vector2 { X: p.x, Y: p.y };
        brush.SetColor(&color_a(
            f.palette.text_secondary,
            (glyph_alpha * 255.0) as u8,
        ));
        let line = 1.6 * s;
        dc.DrawLine(v(a), v(b), brush, line, style.as_ref());
        dc.DrawLine(v(b), v(c), brush, line, style.as_ref());
    }
}

/// 选中块:整行高的胶囊,底色由主题 `selected_bg` 展开成上亮下深。
/// 玻璃再叠贴顶镜面渐变和上下各一道发丝——亮面釉的厚度来自这三层,
/// 不靠描边;实底主题只留很轻的渐变,系统蓝仍是原生样子。
fn paint_selection(
    dc: &ID2D1DeviceContext,
    factory: &ID2D1Factory1,
    rect: panel::Rect,
    f: &Frame,
    brush: &ID2D1SolidColorBrush,
) {
    unsafe {
        let s = f.metrics.scale();
        let radius = rect.height() * 0.5;
        let (top, bottom) = panel::selection_stops(f.palette.selected_bg, f.glass);
        let stops = [
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: color(top),
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: color(bottom),
            },
        ];
        let fill = ID2D1RenderTarget::CreateGradientStopCollection(
            dc,
            &stops,
            D2D1_GAMMA_2_2,
            D2D1_EXTEND_MODE_CLAMP,
        )
        .and_then(|stops| {
            dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 {
                        X: rect.left,
                        Y: rect.top,
                    },
                    endPoint: Vector2 {
                        X: rect.left,
                        Y: rect.bottom,
                    },
                },
                None,
                &stops,
            )
        });
        if let Ok(fill) = fill {
            dc.FillRoundedRectangle(&rrect(rect, radius), &fill);
        } else {
            brush.SetColor(&color(f.palette.selected_bg));
            dc.FillRoundedRectangle(&rrect(rect, radius), brush);
        }
        let (shine, root) = if f.glass {
            (0xfff3cf, 0x6b4300)
        } else {
            (0xffffff, 0x000000)
        };
        if f.glass
            && let Some(geometry) = factory
                .CreateRoundedRectangleGeometry(&rrect(rect, radius))
                .ok()
        {
            // 湿面反光:裁剪在胶囊里。贴边 1px 最亮,顺着顶部圆弧走,
            // 再向中段渐隐;比直线高光更贴合玻璃边缘。
            let gloss_stops = [
                D2D1_GRADIENT_STOP {
                    position: 0.0,
                    color: color_a(shine, 0x66),
                },
                D2D1_GRADIENT_STOP {
                    position: 0.06,
                    color: color_a(shine, 0x34),
                },
                D2D1_GRADIENT_STOP {
                    position: 1.0,
                    color: color_a(shine, 0x00),
                },
            ];
            if let Ok(stops) = ID2D1RenderTarget::CreateGradientStopCollection(
                dc,
                &gloss_stops,
                D2D1_GAMMA_2_2,
                D2D1_EXTEND_MODE_CLAMP,
            ) && let Ok(gloss) = dc.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: Vector2 {
                        X: rect.left,
                        Y: rect.top,
                    },
                    endPoint: Vector2 {
                        X: rect.left,
                        Y: rect.top + rect.height() * 0.58,
                    },
                },
                None,
                &stops,
            ) {
                let mut params = layer_params(&d2d_rect(rect), 1.0);
                params.geometricMask = std::mem::ManuallyDrop::new(Some(geometry.cast().unwrap()));
                dc.PushLayer(&params, None::<&ID2D1Layer>);
                dc.FillRectangle(&d2d_rect(rect), &gloss);
                dc.PopLayer();
            }
        }
        let line = |y: f32, inset: f32, rgb: u32, alpha: u8| {
            brush.SetColor(&color_a(rgb, alpha));
            dc.DrawLine(
                Vector2 {
                    X: rect.left + inset,
                    Y: y,
                },
                Vector2 {
                    X: rect.right - inset,
                    Y: y,
                },
                brush,
                s,
                None::<&ID2D1StrokeStyle>,
            );
        };
        if !f.glass {
            // 实底主题不带釉面层,用一条很淡的顶缘线补轮廓。
            let top_y = rect.top + 1.5 * s;
            line(top_y, panel::capsule_inset(rect, top_y) + s, shine, 0x2e);
        }
        let bottom_y = rect.bottom - 1.5 * s;
        line(
            bottom_y,
            panel::capsule_inset(rect, bottom_y) + s,
            root,
            if f.glass { 0x4d } else { 0x26 },
        );
    }
}

/// 候选内容:高亮块、悬停底色、序号、文字与注音;棋盘所有格统一居中。
/// `alpha` 用于展开形变的交叉淡入,`dy` 把旧内容贴到光标侧。
#[allow(clippy::too_many_arguments)]
fn draw_items(
    dc: &ID2D1DeviceContext,
    factory: &ID2D1Factory1,
    f: &Frame,
    text: &Text,
    brush: &ID2D1SolidColorBrush,
    spec: &PanelSpec,
    width: f32,
    height: f32,
    alpha: f32,
    dy: f32,
    list: panel::ListFx,
    hover: Option<usize>,
) {
    unsafe {
        let s = f.metrics.scale();
        let opacity = (alpha * list.opacity).clamp(0.0, 1.0);
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
        // 滑动/淡入的透明度只作用于列表层,背板与玻璃高光保持稳定。
        if opacity < 1.0 {
            let params = layer_params(
                &D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: width,
                    bottom: height,
                },
                opacity,
            );
            dc.PushLayer(&params, None::<&ID2D1Layer>);
        }
        dc.SetTransform(&Matrix3x2::translation(list.dx, dy));
        if spec.expand && matches!(spec.layout, panel::Layout::Wrap) {
            // 棋盘:页内与页外候选同字号、同格宽,文字在格内居中;
            // 序号与注音不进棋盘,所有列共享同一套排印。
            let cells = panel::grid_cells(spec, &f.metrics, text);
            let draw_cell = |content: &str, cell: &panel::Rect, selected: bool, hovered: bool| {
                if selected {
                    paint_selection(dc, factory, *cell, f, brush);
                } else if hovered {
                    brush.SetColor(&f.tint(f.palette.hover, 0x66));
                    dc.FillRoundedRectangle(&rrect(*cell, cell.height() * 0.5), brush);
                }
                let budget = (cell.width() - 16.0 * s).max(1.0);
                let shown = panel::ellipsize(content, budget, f.metrics.text, text);
                let text_w = text.width(&shown, f.metrics.text);
                brush.SetColor(&color(if selected {
                    f.palette.selected_text
                } else {
                    f.palette.text
                }));
                draw_text(
                    dc,
                    text,
                    &shown,
                    f.metrics.text,
                    &panel::Rect::from_xywh(
                        cell.center_x() - text_w * 0.5,
                        cell.top,
                        text_w.max(1.0) + s,
                        cell.height(),
                    ),
                    brush,
                );
            };
            for (index, (item, cell)) in spec.items.iter().zip(&cells).enumerate() {
                draw_cell(&item.text, cell, item.selected, hover == Some(index));
            }
            for (cell, rect) in spec.grid.iter().zip(cells.iter().skip(spec.items.len())) {
                draw_cell(&cell.text, rect, cell.selected, false);
            }
        } else {
            let rects = panel::item_rects(spec, &f.metrics, text);
            for (index, (item, rect)) in spec.items.iter().zip(&rects).enumerate() {
                let (x, y) = (rect.left, rect.top);
                let row_h = rect.height();
                // 选中块:整行高的胶囊釉面;悬停块只给非选中行。
                if item.selected {
                    paint_selection(dc, factory, *rect, f, brush);
                } else if hover == Some(index) {
                    brush.SetColor(&f.tint(f.palette.hover, 0x66));
                    dc.FillRoundedRectangle(&rrect(*rect, rect.height() * 0.5), brush);
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
                    &panel::Rect::from_xywh(x + panel::ITEM_PAD * s, y, 20.0 * s, row_h),
                    brush,
                );
                brush.SetColor(&color(text_rgb));
                // 固定条:槽宽恒定,文字按槽宽截断(省略号由布局给,所见即所点);
                // 竖排:整词,超宽按 max_text_width 裁。
                let text_x = x + panel::TEXT_X * s;
                let budget = if spec.is_strip() {
                    rect.right - text_x - panel::ITEM_PAD * s
                } else {
                    spec.max_text_width.max(1.0)
                };
                let shown = panel::ellipsize(&item.text, budget, f.metrics.text, text);
                draw_text(
                    dc,
                    text,
                    &shown,
                    f.metrics.text,
                    &panel::Rect::from_xywh(text_x, y, budget.max(1.0), row_h),
                    brush,
                );
                // 注音/来源小字,跟在候选文字后面;固定条没有注音的空间。
                if !spec.is_strip() && !item.hint.is_empty() {
                    let text_width = text.width(&shown, f.metrics.text);
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
        }
        dc.SetTransform(&Matrix3x2::identity());
        if opacity < 1.0 {
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
) {
    unsafe {
        let s = f.metrics.scale();
        let w = 116.0 * s;
        let h = panel::menu_height(&f.metrics);
        let body = panel::Rect::from_xywh(x + 0.5 * s, y + 0.5 * s, w - s, h - s);
        // 菜单是固定形状:外层 10px,行内缩 4px,内层半径 = 10 − 4 = 6。
        brush.SetColor(&color(f.palette.bg));
        dc.FillRoundedRectangle(&rrect(body, 10.0 * s), brush);
        brush.SetColor(&color(f.palette.border));
        dc.DrawRoundedRectangle(
            &rrect(
                panel::Rect::from_xywh(
                    body.left + 0.5 * s,
                    body.top + 0.5 * s,
                    body.width() - s,
                    body.height() - s,
                ),
                9.5 * s,
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
                    panel::concentric_radius(10.0 * s, 4.0 * s, 0.0),
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
