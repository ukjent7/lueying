//! 候选窗的 CPU 呈现:没有硬件 D3D12 时的兜底。
//!
//! 画面与 GPU 路径共用同一套绘制(都在 `ui_skia_gpu::paint` 里),只是落到
//! CPU 光栅表面,再经 GDI 分层窗口上屏。虚拟机、远程桌面这类机器上没有
//! 硬件 D3D12 适配器,候选窗照样弹得出来,代价是每帧多花一点 CPU。
//!
//! 分层窗口不抢焦点、不进 Alt+Tab,淡出期间还能开点击穿透,与 GPU 路径
//! 的观感一致。

use lueying_server::blit::Layer;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

use crate::ui_skia_gpu::{Frame, FrameTarget, paint};

/// CPU 呈现器:光栅表面 + GDI 位图 + 分层窗口。
pub struct CpuPipeline {
    hwnd: HWND,
    layer: Layer,
    /// 光栅表面与位图的尺寸(物理像素)。
    size: (u32, u32),
}

impl CpuPipeline {
    /// 接到一个分层窗口上。
    pub fn new(hwnd: HWND) -> Result<Self, String> {
        let layer = Layer::new()?;
        Ok(Self {
            hwnd,
            layer,
            size: (0, 0),
        })
    }

    /// CPU 路径不碰 GPU,设备永远不会被系统摘掉。
    pub fn device_removed(&self) -> bool {
        false
    }

    /// 尺寸变了就重建光栅表面与位图。
    pub fn rebuild_surfaces(&mut self, w: u32, h: u32) -> Result<(), String> {
        if w == 0 || h == 0 {
            return Err("候选窗尺寸为空".to_string());
        }
        if self.size == (w, h) {
            return Ok(());
        }
        self.layer.surface(w, h)?;
        self.size = (w, h);
        Ok(())
    }

    /// 上屏:位图贴到分层窗口,位置取窗口当前所在的位置。
    pub fn present(&mut self) -> Result<(), String> {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return Err("候选窗尺寸未定".to_string());
        }
        let mut rect = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut rect) }
            .map_err(|e| format!("GetWindowRect: {e}"))?;
        self.layer
            .present(self.hwnd, rect.left, rect.top, w as i32, h as i32)
    }
}

impl FrameTarget for CpuPipeline {
    /// 画一帧到光栅表面;上屏由 `present` 负责。
    fn draw(&mut self, f: Frame) -> Result<(), String> {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return Err("候选窗尺寸未定".to_string());
        }
        {
            let surface = self.layer.surface(w, h)?;
            paint(surface.canvas(), f);
        }
        Ok(())
    }

    /// 读回一个像素(N32 小端字节序是 BGRA,返回 (B, G, R),与 GPU 路径同序)。
    fn pixel(&mut self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        let pixels = self.layer.pixels()?;
        let width = self.size.0 as i32;
        if x < 0 || y < 0 || x >= width {
            return None;
        }
        let index = ((y * width + x) * 4) as usize;
        let bgr = pixels.get(index..index + 3)?;
        Some((bgr[0], bgr[1], bgr[2]))
    }
}
