//! 液态玻璃背板(E 路线,光学配方):逐帧捕获窗口区+外扩(窗口带
//! WDA_EXCLUDEFROMCAPTURE,捕获里不会有自己),透镜里做裁剪 → 折射 →
//! 双高斯渐进模糊 → vibrancy,快照成位图交内容层;背景变动实时跟随。
//!
//! 配方参照 Prismal / liquid_glass_widgets 的 iOS 26 光学栈:模糊本体是
//! 两遍可分离高斯(D2D GaussianBlur 等价),光学感来自配套——bezel 带内
//! 圆弧剖面折射、边缘到中心的渐进变模糊(SDF alpha 环 feather,无硬缝)、
//! 法线镜面 rim、噪点消色带。
//! 尺寸规则:bezel ≤ 0.35 × 短边/2,否则整条变成发光环(廉价感来源)。
//! 效果输出是活引用:每接好一路立即快照进普通位图,不跨接线持有。
//! 裁剪不用 Crop 效果:它的输出坐标会归零,按屏幕坐标平移快照会把内容
//! 移出目标(预览窗口在原点掩盖了这点)。改用普通 DrawImage 把捕获画进
//! 裁剪快照定坐标,效果链只保坐标网格,快照一律零平移。
//! 全文档化 API;捕获失败时调用方回退 DWM 亚克力。

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED,
    D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1ColorMatrix, CLSID_D2D1DisplacementMap, CLSID_D2D1GaussianBlur,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_CHANNEL_SELECTOR_G,
    D2D1_CHANNEL_SELECTOR_R, D2D1_COLORMATRIX_PROP_CLAMP_OUTPUT,
    D2D1_COLORMATRIX_PROP_COLOR_MATRIX, D2D1_DISPLACEMENTMAP_PROP_SCALE,
    D2D1_DISPLACEMENTMAP_PROP_X_CHANNEL_SELECT, D2D1_DISPLACEMENTMAP_PROP_Y_CHANNEL_SELECT,
    D2D1_GAUSSIANBLUR_OPTIMIZATION_QUALITY, D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION,
    D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_PROPERTY_TYPE_BOOL, D2D1_PROPERTY_TYPE_ENUM, D2D1_PROPERTY_TYPE_FLOAT,
    D2D1_PROPERTY_TYPE_MATRIX_5X4, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1Effect, ID2D1Image,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows::core::Interface;
use windows_numerics::Vector2;

/// 身体区高斯标准差(逻辑像素):clear 取向——背景文字不可读但色块结构
/// 仍在;再大就把结构也抹平,整条变均匀色雾=磨砂亚克力观感。
const BODY_STD: f32 = 7.0;
/// 透镜环高斯标准差:bezel 带里较轻模糊,折射弯折才看得见。
const LENS_STD: f32 = 2.0;
/// vibrancy(Rec.709 chroma boost);Prismal 校准值 1.28,过高发糖果色。
const SATURATION: f32 = 1.28;
/// 捕获外扩 = 3 倍身体标准差,模糊边缘不出现透明衰减。
const MARGIN_SIGMAS: f32 = 3.0;
/// 折射带上限(逻辑像素);再受尺寸规则约束:≤ 0.35 × 短边/2。
const BEZEL: f32 = 8.0;
/// 角部折射加强,透镜感集中在圆角。
const CORNER_BOOST: f32 = 1.25;
/// 位移图/alpha 环的分辨率比例;场是平滑的,半分辨率足够。
const MAP_SCALE: f32 = 0.5;

/// 一帧的清晰背板(窗口区+外扩,全分辨率);`luma` 是区域平均亮度(0..1),
/// 材质自适应 dimming 与墨色用。捕获区即窗口区,透镜 rel 恒为 (m, m)。
pub(super) struct Backdrop {
    pub image: ID2D1Image,
    pub luma: f32,
}

/// 一帧的光学背板:身体强模糊 + 透镜环(折射+轻模糊)+ 渐进 alpha 环 + 噪点。
pub(super) struct GlassImages {
    pub body: ID2D1Image,
    pub lens: ID2D1Image,
    pub ring: ID2D1Bitmap1,
    pub grain: ID2D1Bitmap1,
}

/// 身体模糊标准差(物理像素);捕获外扩按它算。
pub(super) fn std(scale: f32) -> f32 {
    BODY_STD * scale
}

/// 捕获外扩边距(物理像素)。
pub(super) fn margin(scale: f32) -> i32 {
    (MARGIN_SIGMAS * std(scale)).ceil() as i32
}

/// 折射带宽(物理像素),受 Prismal 尺寸规则约束。
fn bezel_for(scale: f32, w: u32, h: u32) -> f32 {
    (BEZEL * scale).min(0.35 * w.min(h) as f32 * 0.5)
}

/// 捕获窗口区+外扩边距(全分辨率);任何一步失败回 None。
/// 窗口带 WDA_EXCLUDEFROMCAPTURE,捕获不含自己,无需藏窗,可逐帧调用。
pub(super) fn capture(dc: &ID2D1DeviceContext, hwnd: HWND, scale: f32) -> Option<Backdrop> {
    let m = margin(scale);
    let mut wr = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut wr) }.ok()?;
    let (x, y) = (wr.left - m, wr.top - m);
    let (w, h) = (wr.right - wr.left + 2 * m, wr.bottom - wr.top + 2 * m);
    if w <= 0 || h <= 0 {
        return None;
    }
    let pixels = unsafe { grab(x, y, w, h) }?;
    let luma = region_luma(&pixels, w as usize, h as usize);
    if super::dump_on() {
        let _ = std::fs::write(
            std::env::temp_dir().join("lueying-glass-cap.txt"),
            format!(
                "max={} luma={luma} region={w}x{h} at={x},{y}",
                pixels.iter().max().unwrap_or(&0)
            ),
        );
    }
    let bitmap = upload(
        dc,
        w as u32,
        h as u32,
        &pixels,
        96.0,
        D2D1_ALPHA_MODE_IGNORE,
    )?;
    Some(Backdrop {
        image: bitmap.cast().ok()?,
        luma,
    })
}

/// 区域平均亮度(Rec.709,0..1);隔 4 像素采样,够自适应判断用。
fn region_luma(bgra: &[u8], w: usize, h: usize) -> f32 {
    let mut sum = 0.0;
    let mut n = 0u32;
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let i = (y * w + x) * 4;
            sum += 0.2126 * f32::from(bgra[i + 2])
                + 0.7152 * f32::from(bgra[i + 1])
                + 0.0722 * f32::from(bgra[i]);
            n += 1;
        }
    }
    if n == 0 {
        0.5
    } else {
        sum / (n as f32 * 255.0)
    }
}

/// 透镜:按窗口尺寸缓存位移图、alpha 环、噪点、快照位图与效果链。
pub(super) struct Lens {
    w: u32,
    h: u32,
    scale_bits: u32,
    map: ID2D1Bitmap1,
    ring: ID2D1Bitmap1,
    grain: ID2D1Bitmap1,
    crop_snap: ID2D1Bitmap1,
    body_snap: ID2D1Bitmap1,
    lens_snap: ID2D1Bitmap1,
    disp: ID2D1Effect,
    blur_body: ID2D1Effect,
    blur_lens: ID2D1Effect,
    sat: ID2D1Effect,
}

impl Lens {
    pub(super) fn matches(&self, w: u32, h: u32, scale: f32) -> bool {
        self.w == w && self.h == h && self.scale_bits == scale.to_bits()
    }

    /// 裁出窗口区走光学链;`rel` = 窗口原点 - 捕获原点(物理像素)。
    /// 裁剪靠普通 DrawImage 画进 crop_snap 定坐标;效果链保持坐标网格,
    /// 快照零平移。输出相对窗口(外扩 m),绘制偏移恒为 (-m, -m)。
    pub(super) fn image(
        &self,
        dc: &ID2D1DeviceContext,
        sharp: &ID2D1Image,
        rel: (f32, f32),
        m: f32,
    ) -> Option<GlassImages> {
        let origin = (rel.0 - m, rel.1 - m);
        unsafe {
            snap(dc, &self.crop_snap, sharp, origin)?;
            let cropped: ID2D1Image = self.crop_snap.cast().ok()?;
            self.disp.SetInput(0, &cropped, true);
            self.disp.SetInput(1, &self.map, true);
            let refracted = self.disp.GetOutput().ok()?;
            self.blur_body.SetInput(0, &cropped, true);
            self.blur_lens.SetInput(0, &refracted, true);
            // 每接好一路立即快照;sat 复用,先身体后透镜。
            self.sat
                .SetInput(0, &self.blur_body.GetOutput().ok()?, true);
            snap(dc, &self.body_snap, &self.sat.GetOutput().ok()?, (0.0, 0.0))?;
            self.sat
                .SetInput(0, &self.blur_lens.GetOutput().ok()?, true);
            snap(dc, &self.lens_snap, &self.sat.GetOutput().ok()?, (0.0, 0.0))?;
            Some(GlassImages {
                body: self.body_snap.cast().ok()?,
                lens: self.lens_snap.cast().ok()?,
                ring: self.ring.clone(),
                grain: self.grain.clone(),
            })
        }
    }

    /// 诊断:逐阶段独立快照(预览分层 dump 用)。
    pub(super) fn debug_stages(
        &self,
        dc: &ID2D1DeviceContext,
        m: f32,
    ) -> Vec<(&'static str, ID2D1Bitmap1)> {
        let mut out = vec![("crop", self.crop_snap.clone())];
        unsafe {
            let stages: [(&'static str, ID2D1Image); 3] = [
                ("disp", self.disp.GetOutput().unwrap()),
                ("blurbody", self.blur_body.GetOutput().unwrap()),
                ("blurlens", self.blur_lens.GetOutput().unwrap()),
            ];
            for (tag, img) in stages {
                let Some(bmp) = snap_bitmap(dc, self.w, self.h, m) else {
                    continue;
                };
                if snap(dc, &bmp, &img, (0.0, 0.0)).is_some() {
                    out.push((tag, bmp));
                }
            }
        }
        out
    }
}

/// 建透镜:位移场与 alpha 环半分辨率 CPU 生成一次,效果对象复用。
pub(super) fn lens(
    dc: &ID2D1DeviceContext,
    w: u32,
    h: u32,
    scale: f32,
    radius: f32,
) -> Option<Lens> {
    let m = margin(scale) as f32;
    let bezel = bezel_for(scale, w, h);
    let refract = bezel * 1.2;
    let max_enc = refract * CORNER_BOOST;
    let r = radius.min(w as f32 * 0.5).min(h as f32 * 0.5);
    let (hw, hh) = (w as f32 * 0.5, h as f32 * 0.5);
    let mw = ((w as f32 + 2.0 * m) * MAP_SCALE).ceil() as u32;
    let mh = ((h as f32 + 2.0 * m) * MAP_SCALE).ceil() as u32;
    let mut map_px = vec![0u8; (mw * mh * 4) as usize];
    let mut ring_px = vec![0u8; (mw * mh * 4) as usize];
    for j in 0..mh {
        for i in 0..mw {
            // dpi48:一像素两 DIP;映射到窗口局部坐标。
            let x = (i as f32 + 0.5) / MAP_SCALE - m;
            let y = (j as f32 + 0.5) / MAP_SCALE - m;
            let (px, py) = (x - hw, y - hh);
            let (qx, qy) = (px.abs() - (hw - r), py.abs() - (hh - r));
            let (ox, oy) = (qx.max(0.0), qy.max(0.0));
            let d = (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r;
            let inside = -d;
            // 圆弧剖面:边缘陡、内部平,水珠 meniscus 的弯折分布。
            let t = (1.0 - inside / bezel).clamp(0.0, 1.0);
            let circle = 1.0 - (1.0 - t * t).max(0.0).sqrt();
            let corner = qx > 0.0 && qy > 0.0;
            let mag = refract * circle * if corner { CORNER_BOOST } else { 1.0 };
            let (nx, ny) = if corner {
                let len = (ox * ox + oy * oy).sqrt().max(1e-6);
                (px.signum() * ox / len, py.signum() * oy / len)
            } else if qx > qy {
                (px.signum(), 0.0)
            } else {
                (0.0, py.signum())
            };
            // 渐进 alpha 环:中心 0、边缘 1,smoothstep feather 掉硬缝。
            let tr = (1.0 - inside / (bezel * 1.6)).clamp(0.0, 1.0);
            let a = tr * tr * (3.0 - 2.0 * tr);
            let at = ((j * mw + i) * 4) as usize;
            map_px[at] = ((0.5 + nx * mag / (2.0 * max_enc)) * 255.0) as u8;
            map_px[at + 1] = ((0.5 + ny * mag / (2.0 * max_enc)) * 255.0) as u8;
            map_px[at + 2] = 128;
            map_px[at + 3] = 255;
            // 预乘:RGB 随 alpha 同比,opacityBrush 才读得到渐变。
            let v = (a * 255.0) as u8;
            ring_px[at] = v;
            ring_px[at + 1] = v;
            ring_px[at + 2] = v;
            ring_px[at + 3] = v;
        }
    }
    let map = upload(
        dc,
        mw,
        mh,
        &map_px,
        96.0 * MAP_SCALE,
        D2D1_ALPHA_MODE_IGNORE,
    )?;
    let ring = upload(
        dc,
        mw,
        mh,
        &ring_px,
        96.0 * MAP_SCALE,
        D2D1_ALPHA_MODE_PREMULTIPLIED,
    )?;
    let grain = grain(dc)?;
    let crop_snap = snap_bitmap(dc, w, h, m)?;
    let body_snap = snap_bitmap(dc, w, h, m)?;
    let lens_snap = snap_bitmap(dc, w, h, m)?;
    unsafe {
        let disp = effect(dc, &CLSID_D2D1DisplacementMap)?;
        set_f(&disp, D2D1_DISPLACEMENTMAP_PROP_SCALE.0 as u32, max_enc)?;
        set_enum(
            &disp,
            D2D1_DISPLACEMENTMAP_PROP_X_CHANNEL_SELECT.0 as u32,
            D2D1_CHANNEL_SELECTOR_R.0,
        )?;
        set_enum(
            &disp,
            D2D1_DISPLACEMENTMAP_PROP_Y_CHANNEL_SELECT.0 as u32,
            D2D1_CHANNEL_SELECTOR_G.0,
        )?;
        let blur_body = effect(dc, &CLSID_D2D1GaussianBlur)?;
        set_f(
            &blur_body,
            D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32,
            std(scale),
        )?;
        set_enum(
            &blur_body,
            D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION.0 as u32,
            D2D1_GAUSSIANBLUR_OPTIMIZATION_QUALITY.0,
        )?;
        let blur_lens = effect(dc, &CLSID_D2D1GaussianBlur)?;
        set_f(
            &blur_lens,
            D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32,
            LENS_STD * scale,
        )?;
        set_enum(
            &blur_lens,
            D2D1_GAUSSIANBLUR_PROP_OPTIMIZATION.0 as u32,
            D2D1_GAUSSIANBLUR_OPTIMIZATION_QUALITY.0,
        )?;
        let sat = effect(dc, &CLSID_D2D1ColorMatrix)?;
        set_bytes(
            &sat,
            D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32,
            D2D1_PROPERTY_TYPE_MATRIX_5X4,
            &sat_matrix(SATURATION),
        )?;
        set_bool(&sat, D2D1_COLORMATRIX_PROP_CLAMP_OUTPUT.0 as u32, true)?;
        Some(Lens {
            w,
            h,
            scale_bits: scale.to_bits(),
            map,
            ring,
            grain,
            crop_snap,
            body_snap,
            lens_snap,
            disp,
            blur_body,
            blur_lens,
            sat,
        })
    }
}

/// 把背板铺到内容层;偏移由调用方给(屏幕锚定或窗口相对)。
pub(super) fn draw_backdrop(dc: &ID2D1DeviceContext, image: &ID2D1Image, offset: (f32, f32)) {
    unsafe {
        dc.DrawImage(
            image,
            Some(&Vector2 {
                X: offset.0,
                Y: offset.1,
            }),
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_SOURCE_OVER,
        );
    }
}

/// BGRA 像素上传成 D2D 位图;`dpi` 决定 DIP 尺寸(48 = 一像素两 DIP)。
pub(super) fn upload(
    dc: &ID2D1DeviceContext,
    w: u32,
    h: u32,
    bgra: &[u8],
    dpi: f32,
    alpha: windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE,
) -> Option<ID2D1Bitmap1> {
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: alpha,
        },
        dpiX: dpi,
        dpiY: dpi,
        ..Default::default()
    };
    unsafe {
        dc.CreateBitmap(
            D2D_SIZE_U {
                width: w,
                height: h,
            },
            Some(bgra.as_ptr().cast()),
            w * 4,
            &props,
        )
        .ok()
    }
}

/// 128² 灰噪点 tile;低透明度铺在玻璃上消 8-bit 渐变色带(廉价感来源之一)。
fn grain(dc: &ID2D1DeviceContext) -> Option<ID2D1Bitmap1> {
    let mut px = vec![0u8; 128 * 128 * 4];
    let mut seed = 0x2b992ddfu32;
    for p in px.as_chunks_mut::<4>().0 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let v = 128 + ((seed >> 24) as i32 - 128) / 6;
        p[0] = v as u8;
        p[1] = v as u8;
        p[2] = v as u8;
        p[3] = 255;
    }
    upload(dc, 128, 128, &px, 96.0, D2D1_ALPHA_MODE_IGNORE)
}

/// 饱和倍率 k 的 5x4 颜色矩阵(Rec.709 亮度系数)。D2D 按行向量乘矩阵:
/// 第 i 行是输入通道 i 对各输出的贡献,写转置会把白色染成品红。
fn sat_matrix(k: f32) -> [u8; 80] {
    let (lr, lg, lb) = (0.2126f32, 0.7152, 0.0722);
    let d = 1.0 - k;
    let rows: [f32; 20] = [
        lr * d + k,
        lr * d,
        lr * d,
        0.0, //
        lg * d,
        lg * d + k,
        lg * d,
        0.0, //
        lb * d,
        lb * d,
        lb * d + k,
        0.0, //
        0.0,
        0.0,
        0.0,
        1.0, //
        0.0,
        0.0,
        0.0,
        0.0,
    ];
    let mut bytes = [0u8; 80];
    for (i, v) in rows.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// 把图像按 `-origin` 平移画进目标位图快照:裁剪级传捕获原点,
/// 效果输出保持坐标网格,传零。
unsafe fn snap(
    dc: &ID2D1DeviceContext,
    target: &ID2D1Bitmap1,
    img: &ID2D1Image,
    origin: (f32, f32),
) -> Option<()> {
    unsafe {
        dc.SetTarget(target);
        dc.BeginDraw();
        dc.Clear(None);
        dc.DrawImage(
            img,
            Some(&Vector2 {
                X: -origin.0,
                Y: -origin.1,
            }),
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_SOURCE_OVER,
        );
        if dc.EndDraw(None, None).is_err() {
            dc.SetTarget(None);
            return None;
        }
        dc.SetTarget(None);
        Some(())
    }
}

/// 可作渲染目标的快照位图(窗口区 + 外扩)。
fn snap_bitmap(dc: &ID2D1DeviceContext, w: u32, h: u32, m: f32) -> Option<ID2D1Bitmap1> {
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
        ..Default::default()
    };
    unsafe {
        dc.CreateBitmap(
            D2D_SIZE_U {
                width: (w as f32 + 2.0 * m) as u32,
                height: (h as f32 + 2.0 * m) as u32,
            },
            None,
            0,
            &props,
        )
        .ok()
    }
}

unsafe fn effect(dc: &ID2D1DeviceContext, clsid: &windows::core::GUID) -> Option<ID2D1Effect> {
    unsafe { dc.CreateEffect(clsid).ok() }
}

unsafe fn set_f(e: &ID2D1Effect, i: u32, v: f32) -> Option<()> {
    unsafe {
        e.SetValue(i, D2D1_PROPERTY_TYPE_FLOAT, &v.to_le_bytes())
            .ok()
    }
}

unsafe fn set_enum(e: &ID2D1Effect, i: u32, v: i32) -> Option<()> {
    unsafe {
        e.SetValue(i, D2D1_PROPERTY_TYPE_ENUM, &v.to_le_bytes())
            .ok()
    }
}

unsafe fn set_bool(e: &ID2D1Effect, i: u32, v: bool) -> Option<()> {
    unsafe {
        e.SetValue(i, D2D1_PROPERTY_TYPE_BOOL, &(v as u32).to_le_bytes())
            .ok()
    }
}

unsafe fn set_bytes(
    e: &ID2D1Effect,
    i: u32,
    t: windows::Win32::Graphics::Direct2D::D2D1_PROPERTY_TYPE,
    b: &[u8],
) -> Option<()> {
    unsafe { e.SetValue(i, t, b).ok() }
}

/// 全分辨率截屏一块屏幕区域,返回自上而下 BGRA;GDI 资源当场回收。
unsafe fn grab(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            ReleaseDC(None, screen);
            return None;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let out = CreateDIBSection(Some(screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .ok()
            .and_then(|dib| {
                let old = SelectObject(mem, dib.into());
                let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY).is_ok();
                let data = (ok && !bits.is_null()).then(|| {
                    std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec()
                });
                SelectObject(mem, old);
                let _ = DeleteObject(dib.into());
                data
            });
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        out
    }
}
