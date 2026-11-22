//! 候选窗:系统合成负责背板和裁剪,DirectWrite/Direct2D 绘制内容。

use std::sync::{Arc, Mutex, OnceLock, atomic::Ordering};
use std::time::{Duration, Instant};

use lueying_protocol::{Request, SessionView};
use lueying_render::paint::{Canvas, Color as Ink};
use lueying_render::panel::{self, Estimate, Frame, Hit, Measure, Metrics, Palette, PanelSpec};
use lueying_server::{config::UiLayout, ui_surface::Surface};
use windows::{
    Foundation::{Size, TimeSpan},
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    System::DispatcherQueueController,
    UI::{Color, Composition::*},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::{
            Direct2D::{D2D1CreateDevice, ID2D1DeviceContext},
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
            Direct3D11::{
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
                ID3D11Device,
            },
            Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute},
            Dxgi::IDXGIDevice,
            Gdi::{CreateRoundRectRgn, DeleteObject, SetWindowRgn},
        },
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
            LibraryLoader::GetModuleHandleW,
            WinRT::{
                Composition::{
                    ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop,
                    ICompositorInterop,
                },
                CreateDispatcherQueueController, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
                DispatcherQueueOptions,
            },
        },
        UI::{
            Controls::WM_MOUSELEAVE,
            Input::KeyboardAndMouse::{
                EnableWindow, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
                TrackMouseEvent,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, Interface, Result, h, w},
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use super::candidate_chrome::{Arrow, ArrowPose, Rail, RailPose};
use super::candidate_light::{Ambient, Light};
use super::candidate_material::Material;
use super::place;
use crate::imp::{self, State, Wake};

static WAKE: OnceLock<Wake> = OnceLock::new();
static INPUT: Mutex<std::collections::VecDeque<Pointer>> =
    Mutex::new(std::collections::VecDeque::new());

pub(crate) enum FallbackMouse {
    Down(f32, f32),
    Up,
    Right(f32, f32),
    Wheel(i16),
    Move(f32, f32),
}

enum Pointer {
    Down(f32, f32, bool),
    Move(f32, f32, bool),
    Right(f32, f32, bool),
    Up,
    Wheel(i32),
    Leave,
}

pub(crate) fn kick() {
    if let Some(wake) = WAKE.get() {
        wake.set();
    }
}

fn pointer(event: Pointer) {
    let mut input = INPUT.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(event, Pointer::Move(..)) && matches!(input.back(), Some(Pointer::Move(..))) {
        input.pop_back();
    }
    if input.len() == 32 {
        input.pop_front();
    }
    input.push_back(event);
    drop(input);
    kick();
}

pub(crate) fn forward_mouse(event: FallbackMouse) {
    match event {
        FallbackMouse::Down(x, y) => pointer(Pointer::Down(x, y, true)),
        FallbackMouse::Wheel(delta) => pointer(Pointer::Wheel(delta.into())),
        FallbackMouse::Right(x, y) => pointer(Pointer::Right(x, y, true)),
        FallbackMouse::Move(x, y) => pointer(Pointer::Move(x, y, true)),
        FallbackMouse::Up => pointer(Pointer::Up),
    }
}

fn xy(value: LPARAM) -> (i32, i32) {
    (value.0 as i16 as i32, (value.0 >> 16) as i16 as i32)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lparam);
            unsafe { SetCapture(hwnd) };
            pointer(Pointer::Down(x as f32, y as f32, false));
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lparam);
            unsafe {
                let _ = TrackMouseEvent(&mut TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                });
            }
            pointer(Pointer::Move(x as f32, y as f32, false));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            pointer(Pointer::Leave);
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let (x, y) = xy(lparam);
            pointer(Pointer::Right(x as f32, y as f32, false));
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED => {
            if msg == WM_LBUTTONUP {
                let _ = unsafe { ReleaseCapture() };
            }
            pointer(Pointer::Up);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            pointer(Pointer::Wheel((wparam.0 >> 16) as i16 as i32));
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

struct Window(HWND);

impl Window {
    fn new(layered: bool) -> Result<Self> {
        let name = w!("lueying-composition-candidate");
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            RegisterClassW(&WNDCLASSW {
                hInstance: instance,
                lpszClassName: name,
                lpfnWndProc: Some(window_proc),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            });
            Ok(Self(CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | if layered {
                        WS_EX_LAYERED
                    } else {
                        WS_EX_NOREDIRECTIONBITMAP
                    },
                name,
                name,
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )?))
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

fn graphics(compositor: &Compositor) -> Result<CompositionGraphicsDevice> {
    unsafe {
        let mut device = None::<ID3D11Device>;
        for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
            if D3D11CreateDevice(
                None,
                driver,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .is_ok()
            {
                break;
            }
        }
        let device = device.ok_or_else(|| {
            windows::core::Error::from_hresult(windows::core::HRESULT(0x80004005u32 as i32))
        })?;
        let d2d = D2D1CreateDevice(&device.cast::<IDXGIDevice>()?, None)?;
        compositor
            .cast::<ICompositorInterop>()?
            .CreateGraphicsDevice(&d2d)
    }
}

struct Scene {
    compositor: Compositor,
    root: ContainerVisual,
    _target: Desktop::DesktopWindowTarget,
    backdrop: SpriteVisual,
    color: SpriteVisual,
    tint: CompositionColorBrush,
    geometry: CompositionRoundedRectangleGeometry,
    drawing_interop: ICompositionDrawingSurfaceInterop,
    content: SpriteVisual,
    painter: Surface,
    arrow: Option<Arrow>,
    rail: Option<Rail>,
    glass_available: bool,
    allocation: (i32, i32),
    backdrop_size: Option<(i32, i32)>,
    backdrop_style: Option<(bool, u8)>,
    clip_shape: Option<(f32, f32, f32, f32)>,
}

impl Scene {
    fn new(hwnd: HWND) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let compositor = Compositor::new()?;
        let allowed: BOOL = true.into();
        let glass_available = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_HOSTBACKDROPBRUSH,
                (&allowed as *const BOOL).cast(),
                4,
            )
        }
        .is_ok();
        let target = unsafe {
            compositor
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, true)?
        };
        let root = compositor.CreateContainerVisual()?;
        let geometry = compositor.CreateRoundedRectangleGeometry()?;
        let clip = compositor.CreateGeometricClipWithGeometry(&geometry)?;
        root.SetClip(&clip)?;
        let backdrop = compositor.CreateSpriteVisual()?;
        let glass_available = glass_available
            && compositor
                .CreateHostBackdropBrush()
                .and_then(|brush| backdrop.SetBrush(&brush))
                .is_ok();
        let color = compositor.CreateSpriteVisual()?;
        let tint = compositor.CreateColorBrush()?;
        color.SetBrush(&tint)?;
        let content = compositor.CreateSpriteVisual()?;
        let drawing = graphics(&compositor)?.CreateDrawingSurface(
            Size {
                Width: 1.0,
                Height: 1.0,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        content.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&drawing)?)?;
        let drawing_interop = drawing.cast()?;
        let children = root.Children()?;
        children.InsertAtTop(&backdrop)?;
        children.InsertAtTop(&color)?;
        children.InsertAtTop(&content)?;
        let rail = Rail::new(&compositor, &children).ok();
        let arrow = Arrow::new(&compositor, &children).ok();
        target.SetRoot(&root)?;
        Ok(Self {
            compositor,
            root,
            _target: target,
            backdrop,
            color,
            tint,
            geometry,
            drawing_interop,
            content,
            painter: Surface::new()?,
            arrow,
            rail,
            glass_available,
            allocation: (1, 1),
            backdrop_size: None,
            backdrop_style: None,
            clip_shape: None,
        })
    }

    fn fade(&self, from: Option<f32>, to: f32, millis: i64) -> Result<()> {
        let animation = self.compositor.CreateScalarKeyFrameAnimation()?;
        animation.SetDuration(TimeSpan {
            Duration: millis * 10_000,
        })?;
        if let Some(from) = from {
            animation.InsertKeyFrame(0.0, from)?;
        } else {
            animation.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
        }
        animation.InsertKeyFrame(1.0, to)?;
        self.root.StartAnimation(h!("Opacity"), &animation)
    }

    fn settle_from(&self, y: f32) -> Result<()> {
        let animation = self.compositor.CreateVector3KeyFrameAnimation()?;
        animation.SetDuration(TimeSpan {
            Duration: 1_150_000,
        })?;
        animation.InsertKeyFrame(
            0.0,
            Vector3 {
                X: 0.0,
                Y: y,
                Z: 0.0,
            },
        )?;
        animation.InsertKeyFrame(1.0, Vector3::default())?;
        self.root.StartAnimation(h!("Offset"), &animation)
    }

    fn paint(&mut self, frame: &Layout, motion: Option<&Motion>, light: Light) -> Result<()> {
        let (_, _, w, h) = motion.map_or(frame.rect, |motion| motion.window);
        // A small reusable surface stays allocated across keystrokes and window hides.
        let size = Vector2 {
            X: w as f32,
            Y: h as f32,
        };
        let (old_w, old_h) = self.allocation;
        let (aw, ah) = (old_w.max((w + 63) & !63), old_h.max((h + 31) & !31));
        if self.allocation != (aw, ah) {
            unsafe {
                self.drawing_interop
                    .Resize(windows::Win32::Foundation::SIZE { cx: aw, cy: ah })?
            };
            self.allocation = (aw, ah);
            self.content.SetSize(Vector2 {
                X: aw as f32,
                Y: ah as f32,
            })?;
        }
        let glass = frame.glass && self.glass_available;
        let progress = motion.map(Motion::progress);
        let flex = motion
            .zip(progress)
            .map_or(0.0, |(m, t)| m.glass_flex(frame, t));
        let tint = frame
            .material
            .tint(glass)
            .saturating_sub((flex * 14.0).round() as u8);
        if self.backdrop_size != Some((w, h)) {
            self.backdrop.SetSize(size)?;
            self.color.SetSize(size)?;
            self.backdrop_size = Some((w, h));
        }
        if self.backdrop_style != Some((glass, tint)) {
            self.backdrop.SetIsVisible(glass)?;
            self.tint.SetColor(Color {
                A: tint,
                R: 32,
                G: 28,
                B: 27,
            })?;
            self.backdrop_style = Some((glass, tint));
        }
        let height = motion
            .zip(progress)
            .map_or(h as f32, |(m, t)| m.height(frame, t));
        let offset_y = if motion.is_some_and(|m| m.bottom_anchored) {
            h as f32 - height
        } else {
            0.0
        };
        let radius = panel::PANEL_RADIUS * frame.metrics.scale();
        let shape = (size.X, height, offset_y, radius);
        if self.clip_shape != Some(shape) {
            self.geometry.SetCornerRadius(Vector2 {
                X: radius,
                Y: radius,
            })?;
            self.geometry.SetOffset(Vector2 {
                X: 0.0,
                Y: offset_y,
            })?;
            self.geometry.SetSize(Vector2 {
                X: size.X,
                Y: height,
            })?;
            self.clip_shape = Some(shape);
        }
        if let Some(rail) = &mut self.rail
            && rail
                .set(
                    frame.rail(glass),
                    offset_y,
                    motion.is_some_and(|m| m.resizing),
                )
                .is_err()
        {
            let _ = rail.set(None, 0.0, false);
            self.rail = None;
        }
        if let Some(arrow) = &mut self.arrow
            && arrow
                .set(
                    frame.arrow().map(|mut pose| {
                        pose.rect = offset_rect(pose.rect, 0.0, offset_y);
                        pose
                    }),
                    motion.is_some_and(|m| m.resizing),
                )
                .is_err()
        {
            let _ = arrow.set(None, false);
            self.arrow = None;
        }
        let mut offset = POINT::default();
        let ctx: ID2D1DeviceContext = unsafe { self.drawing_interop.BeginDraw(None, &mut offset)? };
        let rendered = unsafe {
            ctx.Clear(None);
            ctx.SetTransform(&Matrix3x2::translation(
                offset.x as f32,
                offset.y as f32 + offset_y,
            ));
            ctx.SetTextAntialiasMode(
                windows::Win32::Graphics::Direct2D::D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
            );
            self.painter.draw_on(&ctx, |canvas| {
                frame.draw(
                    canvas,
                    glass,
                    motion.zip(progress),
                    height,
                    light,
                    self.arrow.is_none(),
                    self.rail.is_none(),
                    flex,
                );
            })
        };
        let ended = unsafe { self.drawing_interop.EndDraw() };
        rendered.and(ended)
    }
}

struct Layout {
    session: u64,
    spec: PanelSpec,
    metrics: Metrics,
    palette: Palette,
    rect: (i32, i32, i32, i32),
    glass: bool,
    material: Material,
    effects: bool,
    menu: Option<usize>,
    raw: String,
    page: usize,
    page_start: usize,
    private: bool,
    hover: Hover,
    above: bool,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Hover {
    item: Option<usize>,
    arrow: bool,
    scroll: bool,
    menu: Option<usize>,
}

#[derive(Clone, PartialEq)]
struct Cell {
    text: String,
    hint: String,
    index: usize,
    rect: panel::Rect,
    selected: bool,
    leading: f32,
    slot: Option<usize>,
}

struct MovingCell {
    target: Cell,
    origin: Cell,
    opacity: Opacity,
}

#[derive(Clone, Copy)]
struct Opacity {
    start: Instant,
    delay: Duration,
    duration: Duration,
    from: f32,
    to: f32,
}

impl Opacity {
    fn new(now: Instant, from: f32, to: f32, delay: u64, millis: u64) -> Self {
        Self {
            start: now,
            delay: Duration::from_millis(delay),
            duration: Duration::from_millis(millis),
            from,
            to,
        }
    }

    fn at(self, now: Instant) -> f32 {
        if self.from == self.to {
            return self.to;
        }
        let elapsed = now
            .saturating_duration_since(self.start)
            .saturating_sub(self.delay);
        let t = (elapsed.as_secs_f32() / self.duration.as_secs_f32()).min(1.0);
        let ease = t * t * (3.0 - 2.0 * t);
        self.from + (self.to - self.from) * ease
    }
}

impl MovingCell {
    fn current(&self, t: f32) -> Cell {
        let mut cell = self.target.clone();
        cell.rect = interp(self.origin.rect, cell.rect, t);
        cell.leading = self.origin.leading + (cell.leading - self.origin.leading) * t;
        cell
    }
}

struct Motion {
    start: Instant,
    window: (i32, i32, i32, i32),
    from_height: f32,
    from_flex: f32,
    cells: Vec<MovingCell>,
    capsule: Option<(panel::Rect, panel::Rect)>,
    duration: Duration,
    geometry_duration: Duration,
    resizing: bool,
    bottom_anchored: bool,
    strip_swap: bool,
}

impl Motion {
    fn follows_strip_caret(from: &Layout, to: &Layout, previous: Option<&Self>) -> bool {
        let drift_limit = (48.0 * to.metrics.scale()).round() as u32;
        from.session == to.session
            && to.effects
            && !from.private
            && !to.private
            && from.menu.is_none()
            && to.menu.is_none()
            && from.spec.is_strip()
            && to.spec.is_strip()
            && from.metrics.scale() == to.metrics.scale()
            && from.rect.2 == to.rect.2
            && from.rect.3 == to.rect.3
            && from.above == to.above
            && from.rect.0.abs_diff(to.rect.0) <= drift_limit
            && from.rect.1.abs_diff(to.rect.1) <= drift_limit
            && previous.is_none_or(|motion| !motion.resizing)
    }

    fn progress(&self) -> f32 {
        let elapsed = self.start.elapsed();
        if elapsed >= self.geometry_duration {
            return 1.0;
        }
        let t = elapsed.as_secs_f32() * 0.310 / self.geometry_duration.as_secs_f32();
        // Underdamped step response. Snap the small remainder at the end of the transition.
        1.0 - (-17.0 * t).exp() * ((21.0 * t).cos() + (17.0 / 21.0) * (21.0 * t).sin())
    }

    fn height(&self, to: &Layout, t: f32) -> f32 {
        (self.from_height + (to.rect.3 as f32 - self.from_height) * t)
            .clamp(1.0, self.window.3 as f32)
    }

    fn capsule(&self, t: f32) -> Option<panel::Rect> {
        self.capsule.map(|(a, b)| interp(a, b, t))
    }

    fn glass_flex(&self, frame: &Layout, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let carried = self.from_flex * (1.0 - t);
        if self.resizing {
            let distance = (frame.rect.3 as f32 - self.from_height).abs();
            let pulse = (distance / (100.0 * frame.metrics.scale())).min(1.0) * 4.0 * t * (1.0 - t);
            carried + (1.0 - carried) * pulse
        } else {
            carried
        }
    }

    fn new(from: &Layout, to: &Layout, previous: Option<&Self>) -> Option<Self> {
        let same_top = from.rect.1 == to.rect.1;
        let same_bottom = from.rect.1 + from.rect.3 == to.rect.1 + to.rect.3;
        let compact = from.spec.is_strip() && to.spec.is_strip();
        let follow_caret = Self::follows_strip_caret(from, to, previous);
        if !to.effects
            || from.session != to.session
            || from.private
            || to.private
            || from.spec.layout != to.spec.layout
            || from.menu.is_some()
            || to.menu.is_some()
            || (!follow_caret && from.rect.0 != to.rect.0)
            || from.rect.2 != to.rect.2
            || (!follow_caret && !same_top && !same_bottom)
            || from.metrics.scale() != to.metrics.scale()
            || matches!((from.spec.scroll, to.spec.scroll), (Some(a), Some(b)) if a.offset != b.offset)
        {
            return None;
        }
        let from_cells = from.cells();
        let to_cells = to.cells();
        let resizing = from.spec.expand != to.spec.expand || from.rect.3 != to.rect.3;
        let paging = from.raw == to.raw && from.page != to.page && !to.spec.expand;
        let strip_swap = compact && !resizing && !paging;
        let now = Instant::now();
        if !resizing && !paging && from_cells == to_cells {
            return None;
        }
        let slide = if paging {
            if to.page > from.page {
                to.rect.2 as f32
            } else {
                -(to.rect.2 as f32)
            }
        } else {
            0.0
        };
        let selected = |cells: &[Cell]| {
            cells
                .iter()
                .find(|cell| cell.selected)
                .map(|cell| cell.rect)
        };
        let capsule = selected(&to_cells).map(|target| {
            let origin = if paging {
                offset_rect(target, slide, 0.0)
            } else {
                previous
                    .and_then(|m| m.capsule(m.progress()))
                    .or_else(|| selected(&from_cells))
                    .unwrap_or(target)
            };
            (origin, target)
        });
        let origins: Vec<_> = previous.map_or_else(
            || {
                from_cells
                    .into_iter()
                    .map(|cell| (cell, Opacity::new(now, 1.0, 1.0, 0, 1)))
                    .collect()
            },
            |m| {
                let t = m.progress();
                m.cells
                    .iter()
                    .filter(|cell| cell.opacity.to > 0.0 || cell.opacity.at(now) > 0.0)
                    .map(|cell| (cell.current(t), cell.opacity))
                    .collect()
            },
        );
        let mut origins = origins.into_iter().map(Some).collect::<Vec<_>>();
        let mut cells = to_cells
            .into_iter()
            .map(|cell| {
                let old = if paging {
                    None
                } else {
                    origins
                        .iter_mut()
                        .find(|old| {
                            old.as_ref().is_some_and(|(old, _)| {
                                old.index == cell.index && old.text == cell.text
                            })
                        })
                        .and_then(Option::take)
                };
                let (origin, opacity) = old.map_or_else(
                    || {
                        let mut origin = cell.clone();
                        origin.rect = offset_rect(
                            cell.rect,
                            slide,
                            if resizing {
                                -12.0 * to.metrics.scale()
                            } else {
                                0.0
                            },
                        );
                        let replacing = strip_swap
                            && origins
                                .iter()
                                .flatten()
                                .any(|(old, fade)| old.index == cell.index && fade.at(now) >= 0.25);
                        let fade = if paging {
                            Opacity::new(now, 1.0, 1.0, 0, 1)
                        } else if strip_swap {
                            let initial = if !replacing && previous.is_some() {
                                0.16
                            } else {
                                0.0
                            };
                            Opacity::new(now, initial, 1.0, if replacing { 35 } else { 0 }, 155)
                        } else {
                            Opacity::new(now, 0.0, 1.0, 0, 145)
                        };
                        (origin, fade)
                    },
                    |(origin, fade)| {
                        let fade = if fade.to == 1.0 {
                            fade
                        } else {
                            Opacity::new(now, fade.at(now), 1.0, 0, 135)
                        };
                        (origin, fade)
                    },
                );
                MovingCell {
                    target: cell,
                    origin,
                    opacity,
                }
            })
            .collect::<Vec<_>>();
        // A rapidly reused slot keeps at most one visible outgoing word, never a trail.
        if strip_swap {
            origins.sort_by(|a, b| {
                let alpha = |entry: &Option<(Cell, Opacity)>| {
                    entry.as_ref().map_or(0.0, |(_, fade)| fade.at(now))
                };
                alpha(b).total_cmp(&alpha(a))
            });
        }
        let mut outgoing_slots = Vec::new();
        for (origin, fade) in origins.into_iter().flatten().take(32) {
            let alpha = fade.at(now);
            if alpha <= 0.0 || (strip_swap && outgoing_slots.contains(&origin.index)) {
                continue;
            }
            if strip_swap {
                outgoing_slots.push(origin.index);
            }
            let mut target = origin.clone();
            target.rect = offset_rect(
                origin.rect,
                -slide,
                if resizing {
                    -12.0 * to.metrics.scale()
                } else {
                    0.0
                },
            );
            let opacity = if fade.to == 0.0 {
                fade
            } else {
                Opacity::new(now, alpha, 0.0, 0, if strip_swap { 112 } else { 145 })
            };
            cells.push(MovingCell {
                target,
                origin,
                opacity,
            });
        }
        let y = from
            .rect
            .1
            .min(to.rect.1)
            .min(previous.map_or(to.rect.1, |motion| motion.window.1));
        let bottom = (from.rect.1 + from.rect.3)
            .max(to.rect.1 + to.rect.3)
            .max(previous.map_or(0, |motion| motion.window.1 + motion.window.3));
        let geometry_duration = Duration::from_millis(if resizing {
            310
        } else if paging {
            195
        } else {
            145
        });
        let duration = if strip_swap
            && cells
                .iter()
                .any(|cell| cell.opacity.from != cell.opacity.to)
        {
            Duration::from_millis(190)
        } else {
            geometry_duration
        };
        Some(Self {
            start: now,
            window: if follow_caret {
                to.rect
            } else {
                (to.rect.0, y, to.rect.2, bottom - y)
            },
            from_height: previous.map_or(from.rect.3 as f32, |m| m.height(from, m.progress())),
            from_flex: previous.map_or(0.0, |m| m.glass_flex(from, m.progress())),
            cells,
            capsule,
            duration,
            geometry_duration,
            resizing,
            bottom_anchored: !follow_caret && !same_top && same_bottom,
            strip_swap,
        })
    }
}

fn offset_rect(r: panel::Rect, x: f32, y: f32) -> panel::Rect {
    panel::Rect::from_xywh(r.left + x, r.top + y, r.width(), r.height())
}

fn interp(a: panel::Rect, b: panel::Rect, t: f32) -> panel::Rect {
    let mix = |x, y| x + (y - x) * t;
    panel::Rect::from_xywh(
        mix(a.left, b.left),
        mix(a.top, b.top),
        f32::max(1.0, mix(a.width(), b.width())),
        f32::max(1.0, mix(a.height(), b.height())),
    )
}

impl Layout {
    fn rail(&self, glass: bool) -> Option<RailPose> {
        let cap = panel::chevron_rect(&self.spec, &self.metrics, &Estimate)?;
        let height = self.rect.3 as f32;
        let rail = panel::rail_rect(&self.spec, &self.metrics, &Estimate, height);
        let scroll = self.spec.scroll.and_then(|scroll| {
            panel::scrollbar_rects(
                scroll,
                self.rect.2 as f32,
                height,
                &self.metrics,
                self.hover.scroll,
            )
        });
        Some(RailPose {
            cap,
            rail,
            scroll,
            ink: self.palette.text_tertiary,
            border: self.palette.border,
            hover_cap: self.hover.arrow,
            hover_scroll: self.hover.scroll,
            expanded: self.spec.expand,
            glass,
        })
    }

    fn arrow(&self) -> Option<ArrowPose> {
        let rect = panel::chevron_rect(&self.spec, &self.metrics, &Estimate)?;
        let small = self.spec.expand
            && self
                .spec
                .scroll
                .is_some_and(|scroll| scroll.cap > 0 && scroll.len > scroll.cap);
        Some(ArrowPose {
            rect,
            scale: self.metrics.scale(),
            expanded: self.spec.expand,
            rgb: self.palette.text_secondary,
            alpha: if small && !self.hover.arrow {
                0x8c
            } else {
                0xff
            },
        })
    }

    fn new(
        state: &State,
        session: u64,
        view: &SessionView,
        menu: Option<usize>,
        patch: Option<u32>,
        private: bool,
        prefer_above: Option<bool>,
    ) -> Option<Self> {
        let (l, t, r, b) = view.caret?;
        if place::foreground_fullscreen_at(l, b, (session >> 32) as u32) {
            return None;
        }
        let dpi = place::scale_at(l, b);
        let (layout, font, accent) = place::ui_options(state);
        let metrics = Metrics::scaled(dpi * font.factor());
        let (mut spec, fit) = place::spec_of(view, layout, &metrics, &Estimate);
        if !view.expanded && matches!(layout, UiLayout::Horizontal) && fit != view.page_size {
            imp::handle(state, Request::SetPageSize { session, size: fit });
        }
        spec.max_text_width = 490.0 * dpi * font.factor();
        spec.width = place::measure_width(view, layout, &metrics, &Estimate);
        let area = place::work_area_at(l, b);
        let w = (panel::panel_width(&spec, &metrics, &Estimate).ceil() as i32)
            .min(area.right - area.left)
            .max(1);
        let h = panel::panel_height(&spec, &metrics, &Estimate)
            .max(menu.map_or(0.0, |_| {
                panel::menu_height(&metrics) + 8.0 * metrics.scale()
            }))
            .ceil()
            .max(1.0) as i32;
        let gap = (place::CARET_GAP * dpi) as i32;
        let reserve = if layout == UiLayout::Horizontal {
            h.max(
                (2.0 * panel::PAD_X * metrics.scale()
                    + (panel::GRID_ROWS - 1) as f32 * metrics.row
                    + metrics.visual_row())
                .ceil() as i32,
            )
        } else {
            h
        };
        let fits_below = b + reserve + gap <= area.bottom;
        let fits_above = t - reserve - gap >= area.top;
        let above = match prefer_above {
            Some(true) if fits_above => true,
            Some(false) if fits_below => false,
            _ if fits_above || fits_below => !fits_below,
            _ => b + h + gap > area.bottom && t - h - gap >= area.top,
        };
        state.candidate_above.store(above, Ordering::Relaxed);
        let x = l.min(r).clamp(area.left, (area.right - w).max(area.left));
        let y = (if above { t - h - gap } else { b + gap })
            .clamp(area.top, (area.bottom - h).max(area.top));
        let cfg = state.config.read().unwrap_or_else(|p| p.into_inner());
        let effects = cfg.effects && !lueying_server::theme::client_area_animation();
        use lueying_server::{
            config::UiMaterial,
            theme::{self, Appearance},
        };
        let appearance = Appearance::of_system();
        let look = if appearance == Appearance::HighContrast {
            appearance
        } else {
            Appearance::Dark
        };
        let colors = patch.map_or_else(
            || theme::colors_with(look, accent),
            |rgb| theme::colors_with_accent(look, rgb),
        );
        let mut palette = theme::palette(colors);
        let material = if appearance == Appearance::HighContrast {
            Material::Plain
        } else {
            Material::new(cfg.ui_material, cfg.ui_glass_frost)
        };
        material.palette(&mut palette, patch.is_some());
        if cfg.ui_material == UiMaterial::Glass && appearance != Appearance::HighContrast {
            if patch.is_none() {
                palette.selected_bg = 0xfac738;
            }
            palette.selected_text = 0x201807;
        }
        Some(Self {
            session,
            spec,
            metrics,
            palette,
            rect: (x, y, w, h),
            glass: cfg.ui_material.is_glass()
                && cfg.effects
                && appearance != Appearance::HighContrast,
            material,
            effects,
            menu,
            raw: view.raw.clone(),
            page: view.page,
            page_start: panel::page_bounds(view.page, view.page_size, view.candidates.len()).start,
            private,
            hover: Hover::default(),
            above,
        })
    }

    fn cells(&self) -> Vec<Cell> {
        if self.spec.expand && self.spec.layout == panel::Layout::Wrap {
            self.spec
                .grid
                .iter()
                .zip(panel::grid_cells(&self.spec, &self.metrics, &Estimate))
                .map(|(item, rect)| Cell {
                    text: item.text.clone(),
                    hint: String::new(),
                    index: item.index,
                    rect,
                    selected: item.selected,
                    leading: 0.0,
                    slot: None,
                })
                .collect()
        } else {
            self.spec
                .items
                .iter()
                .zip(panel::item_rects(&self.spec, &self.metrics, &Estimate))
                .enumerate()
                .map(|(slot, (item, rect))| Cell {
                    text: item.text.clone(),
                    hint: item.hint.clone(),
                    index: self.page_start + slot,
                    rect,
                    selected: item.selected,
                    leading: 1.0,
                    slot: Some(slot),
                })
                .collect()
        }
    }

    fn menu_rect(&self, slot: usize) -> panel::Rect {
        let s = self.metrics.scale();
        let height = panel::menu_height(&self.metrics);
        let rects = panel::item_rects(&self.spec, &self.metrics, &Estimate);
        let y = rects.get(slot).map_or(4.0 * s, |row| {
            if row.bottom + height <= self.rect.3 as f32 {
                row.bottom
            } else {
                (row.top - height).max(4.0 * s)
            }
        });
        panel::Rect::from_xywh(8.0 * s, y, 116.0 * s, height)
    }

    fn draw(
        &self,
        canvas: &mut dyn Canvas,
        glass: bool,
        motion: Option<(&Motion, f32)>,
        h: f32,
        light: Light,
        draw_arrow: bool,
        draw_rail: bool,
        flex: f32,
    ) {
        let w = self.rect.2;
        let s = self.metrics.scale();
        let surface = self
            .material
            .underlay(canvas, &self.metrics, w, h, glass, light, flex);
        if surface == panel::Surface::Solid && self.menu.is_some() {
            canvas.fill_rrect(
                panel::Rect::from_xywh(0.0, 0.0, w as f32, h),
                panel::PANEL_RADIUS * s,
                Ink::opaque(self.palette.bg),
            );
        }
        panel::draw_with_items(
            canvas,
            &Frame {
                spec: &self.spec,
                metrics: self.metrics,
                palette: &self.palette,
                surface,
                hover: self.hover.item,
                chev: self.hover.arrow,
                scroll_hover: self.hover.scroll,
            },
            &Estimate,
            motion.is_none(),
            draw_arrow,
            draw_rail,
        );
        if let Some((motion, t)) = motion {
            if let Some(capsule) = motion.capsule(t) {
                panel::draw_selection_capsule(
                    canvas,
                    capsule,
                    s,
                    self.palette.selected_bg,
                    glass,
                    255,
                );
            }
            let now = Instant::now();
            for cell in motion.cells.iter().rev() {
                let rect = interp(cell.origin.rect, cell.target.rect, t);
                let alpha = (cell.opacity.at(now) * 255.0).round() as u8;
                let leading = (cell.origin.leading
                    + (cell.target.leading - cell.origin.leading) * t)
                    .clamp(0.0, 1.0);
                if let Some(slot) = cell.target.slot.or(cell.origin.slot)
                    && leading > 0.0
                    && (!motion.strip_swap || cell.opacity.to == 1.0)
                {
                    canvas.text(
                        &format!("{}", slot + 1),
                        lueying_render::paint::Point::new(
                            rect.left + panel::ITEM_PAD * s,
                            rect.center_y(),
                        ),
                        self.metrics.small,
                        Ink::with_alpha(
                            if cell.target.selected {
                                self.palette.accent_text
                            } else {
                                self.palette.text_tertiary
                            },
                            if motion.strip_swap {
                                255
                            } else {
                                (f32::from(alpha) * leading).round() as u8
                            },
                        ),
                        Some(20.0 * s),
                    );
                }
                if alpha == 0 {
                    continue;
                }
                let margin = (8.0 + (panel::TEXT_X + panel::ITEM_PAD - 8.0) * leading) * s;
                let budget = (rect.width() - margin).max(1.0);
                let text =
                    panel::ellipsize(&cell.target.text, budget, self.metrics.text, &Estimate);
                let width = Estimate.width(&text, self.metrics.text);
                let center = rect.center_x() - width * 0.5;
                let x = center + (rect.left + panel::TEXT_X * s - center) * leading;
                canvas.text(
                    &text,
                    lueying_render::paint::Point::new(x, rect.center_y()),
                    self.metrics.text,
                    Ink::with_alpha(
                        if cell.target.selected {
                            self.palette.selected_text
                        } else {
                            self.palette.text
                        },
                        alpha,
                    ),
                    Some(budget),
                );
                if !cell.target.hint.is_empty() {
                    canvas.text(
                        &cell.target.hint,
                        lueying_render::paint::Point::new(x + width + 4.0 * s, rect.center_y()),
                        self.metrics.small,
                        Ink::with_alpha(self.palette.text_tertiary, alpha),
                        Some(100.0 * s),
                    );
                }
            }
        }
        if let Some(slot) = self.menu {
            let rect = self.menu_rect(slot);
            canvas.fill_rrect(rect, 8.0 * s, Ink::opaque(self.palette.surface_alt));
            canvas.stroke_rrect(rect, 8.0 * s, s, Ink::opaque(self.palette.border));
            for (i, label) in ["\u{7f6e}\u{9876}", "\u{964d}\u{6743}"]
                .into_iter()
                .enumerate()
            {
                if self.hover.menu == Some(i) {
                    canvas.fill_rrect(
                        panel::Rect::from_xywh(
                            rect.left + 4.0 * s,
                            rect.top + (4.0 + 28.0 * i as f32) * s,
                            rect.width() - 8.0 * s,
                            26.0 * s,
                        ),
                        5.0 * s,
                        Ink::opaque(self.palette.hover),
                    );
                }
                canvas.text(
                    label,
                    lueying_render::paint::Point::new(
                        rect.left + 10.0 * s,
                        rect.top + (17.0 + 28.0 * i as f32) * s,
                    ),
                    self.metrics.text,
                    Ink::opaque(self.palette.text),
                    None,
                );
            }
        }
    }

    fn hover_at(&self, x: f32, y: f32, screen: bool) -> Hover {
        let (x, y) = if screen {
            (x - self.rect.0 as f32, y - self.rect.1 as f32)
        } else {
            (x, y)
        };
        if let Some(slot) = self.menu {
            let rect = self.menu_rect(slot);
            return Hover {
                menu: panel::menu_hit(rect.left, rect.top, &self.metrics, x, y),
                ..Hover::default()
            };
        }
        let hit = panel::hit_test(&self.spec, &self.metrics, &Estimate, x, y);
        let scroll = hit != Hit::Expand
            && self.spec.scroll.is_some_and(|scroll| {
                panel::scrollbar_hit(
                    scroll,
                    self.rect.2 as f32,
                    self.rect.3 as f32,
                    &self.metrics,
                    x,
                    y,
                )
                .is_some()
            });
        let item = if scroll {
            None
        } else {
            match hit {
                Hit::Item(index) => Some(index),
                Hit::GridItem(index) => self
                    .spec
                    .grid
                    .iter()
                    .position(|item| item.index == index)
                    .map(|i| self.spec.items.len() + i),
                _ => None,
            }
        };
        Hover {
            item,
            arrow: hit == Hit::Expand,
            scroll,
            menu: None,
        }
    }

    fn respond(
        &self,
        state: &State,
        event: Pointer,
        drag: &mut Option<f32>,
        menu: &mut Option<usize>,
    ) -> bool {
        if state.active.load(Ordering::Relaxed) != self.session {
            return false;
        }
        let session = self.session;
        let (x, y, screen, kind) = match event {
            Pointer::Wheel(delta) => {
                if delta != 0 {
                    if self.spec.expand {
                        imp::handle(
                            state,
                            Request::Page {
                                session,
                                delta: -delta.signum(),
                            },
                        );
                    } else if self.spec.layout == panel::Layout::Wrap && self.spec.items.len() > 1 {
                        imp::handle(state, Request::Expand { session });
                    }
                }
                return false;
            }
            Pointer::Down(x, y, s) => (x, y, s, 0),
            Pointer::Move(x, y, s) => (x, y, s, 1),
            Pointer::Right(x, y, s) => (x, y, s, 2),
            Pointer::Up => {
                *drag = None;
                return false;
            }
            Pointer::Leave => return false,
        };
        let (x, y) = if screen {
            (x - self.rect.0 as f32, y - self.rect.1 as f32)
        } else {
            (x, y)
        };
        let (w, h) = (self.rect.2 as f32, self.rect.3 as f32);
        let hit = panel::hit_test(&self.spec, &self.metrics, &Estimate, x, y);
        if let Some(scroll) = self.spec.scroll {
            if kind == 0
                && !matches!(hit, Hit::Expand)
                && let Some(hit) = panel::scrollbar_hit(scroll, w, h, &self.metrics, x, y)
                && let Some((_, thumb)) = panel::scrollbar_rects(scroll, w, h, &self.metrics, true)
            {
                let grab = if hit == panel::ScrollHit::Thumb {
                    y - thumb.top
                } else {
                    thumb.height() / 2.0
                };
                *drag = Some(grab);
            }
            if kind < 2
                && let Some(grab) = *drag
            {
                let offset = panel::scrollbar_offset(scroll, w, h, &self.metrics, y, Some(grab));
                if offset != scroll.offset {
                    imp::handle(state, Request::ScrollTo { session, offset });
                }
                return false;
            }
        }
        if kind == 1 {
            return false;
        }
        if kind == 2 {
            *menu = if let Hit::Item(slot) = hit {
                Some(slot)
            } else {
                None
            };
            return true;
        }
        if let Some(slot) = menu.take() {
            let rect = self.menu_rect(slot);
            if let Some(item) = panel::menu_hit(rect.left, rect.top, &self.metrics, x, y) {
                let req = if item == 0 {
                    Request::Pin { session, slot }
                } else {
                    Request::Demote { session, slot }
                };
                imp::handle(state, req);
            }
            return true;
        }
        let request = match hit {
            Hit::Expand => Request::Expand { session },
            Hit::Item(slot) => Request::Pick { session, slot },
            Hit::GridItem(index) => Request::PickIndex { session, index },
            _ => return false,
        };
        imp::handle(state, request);
        imp::fallback::flush(state);
        false
    }
}

enum Renderer {
    Composed(Box<Scene>),
    Solid(Surface),
}

impl Renderer {
    fn enter(&self, hwnd: HWND, frame: &Layout, reversing: bool) {
        let _ = unsafe { EnableWindow(hwnd, true) };
        if let Self::Composed(scene) = self {
            if frame.effects
                && !frame.private
                && scene.fade((!reversing).then_some(0.0), 1.0, 115).is_ok()
            {
                if !reversing {
                    let sign = if frame.above { 1.0 } else { -1.0 };
                    let _ = scene.settle_from(sign * 6.0 * frame.metrics.scale());
                }
                return;
            }
            let _ = scene.root.StopAnimation(h!("Opacity"));
            let _ = scene.root.SetOpacity(1.0);
        }
    }

    fn exit(&self, hwnd: HWND) -> bool {
        if let Self::Composed(scene) = self
            && scene.fade(None, 0.0, 90).is_ok()
        {
            let _ = unsafe { EnableWindow(hwnd, false) };
            true
        } else {
            false
        }
    }

    fn hide(&mut self) {
        if let Self::Composed(scene) = self {
            if let Some(arrow) = &mut scene.arrow {
                let _ = arrow.set(None, false);
            }
            if let Some(rail) = &mut scene.rail {
                let _ = rail.set(None, 0.0, false);
            }
        }
    }

    fn paint(
        &mut self,
        hwnd: HWND,
        frame: &Layout,
        motion: Option<&Motion>,
        light: Light,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        match self {
            Self::Composed(scene) => scene.paint(frame, motion, light)?,
            Self::Solid(painter) => {
                let (x, y, w, h) = frame.rect;
                frame.draw(
                    painter.begin(w as u32, h as u32)?,
                    false,
                    None,
                    h as f32,
                    light,
                    true,
                    true,
                    0.0,
                );
                painter.present(hwnd, x, y, w, h)?;
            }
        }
        Ok(())
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if let Self::Composed(scene) = self {
            let _ = scene._target.SetRoot(None::<&Visual>);
        }
    }
}

fn publish(state: &State, frame: Option<(&Layout, &place::Snapshot)>) {
    use lueying_ext::proto::{SceneIn, candidate::CandidateState};
    let Some((layout, snap)) = frame else {
        state.ext.publish_candidates(CandidateState::default());
        return;
    };
    let view = &snap.view;
    let (x, y, w, h) = layout.rect;
    let mut status = CandidateState {
        visible: true,
        composing: true,
        private: snap.private,
        rect: [x, y, w, h],
        ..Default::default()
    };
    if !snap.private {
        status.raw.clone_from(&view.raw);
        status.page = view.page;
        status.page_size = view.page_size;
        status.highlight = view.highlight;
        status.candidates.clone_from(&view.candidates);
    }
    state.ext.publish_candidates(status);
    let page_size = view.page_size.max(1);
    let page = panel::page_bounds(view.page, page_size, view.candidates.len());
    let appearance = lueying_server::theme::Appearance::of_system();
    state.ext.submit_scene(SceneIn {
        composing: true,
        page: (view.page + 1) as u32,
        pages: view.candidates.len().div_ceil(page_size).max(1) as u32,
        candidates: page.len() as u32,
        selected: view
            .highlight
            .saturating_sub(page.start)
            .min(page.len().saturating_sub(1)) as u32,
        accent: layout.palette.accent,
        dark: appearance == lueying_server::theme::Appearance::Dark,
        high_contrast: appearance == lueying_server::theme::Appearance::HighContrast,
        reduce_motion: !lueying_server::theme::client_area_animation(),
        private: snap.private,
        ..Default::default()
    });
}

fn clear_candidate(state: &State) {
    *state
        .candidate_rect
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    state.candidate_above.store(false, Ordering::Relaxed);
}

fn show(window: HWND, rect: (i32, i32, i32, i32), scale: f32) -> Result<()> {
    let (x, y, w, h) = rect;
    unsafe {
        let diameter = (2.0 * panel::PANEL_RADIUS * scale).round() as i32;
        let region = CreateRoundRectRgn(0, 0, w + 1, h + 1, diameter, diameter);
        if SetWindowRgn(window, Some(region), false) == 0 {
            let _ = DeleteObject(region.into());
        }
        SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            x,
            y,
            w,
            h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
}

fn event_loop(state: &State, layered: bool) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let wake = *WAKE.get_or_init(Wake::create);
    let mut version = u64::MAX;
    let mut panel = None::<(Renderer, Window)>;
    let mut rendered = None::<Layout>;
    let mut hidden_at = None::<Instant>;
    let mut fading_until = None::<Instant>;
    let mut fading_session = 0;
    let mut menu = None;
    let mut drag = None;
    let mut accent = None;
    let mut motion = None::<Motion>;
    let mut ambient = Ambient::default();
    let mut sample_light = false;
    loop {
        super::pump_messages();
        let input = std::mem::take(&mut *INPUT.lock().unwrap_or_else(|e| e.into_inner()));
        let mut dirty = false;
        for event in input {
            if let Some(frame) = &mut rendered {
                let hover = match event {
                    Pointer::Move(x, y, screen) => Some(frame.hover_at(x, y, screen)),
                    Pointer::Leave => Some(Hover::default()),
                    _ => None,
                };
                if let Some(hover) = hover
                    && hover != frame.hover
                {
                    frame.hover = hover;
                    dirty = true;
                }
                if motion.is_some() && !matches!(event, Pointer::Up) {
                    continue;
                }
                if version != state.version.load(Ordering::Acquire)
                    && !matches!(event, Pointer::Up | Pointer::Move(..))
                {
                    continue;
                }
                dirty |= frame.respond(state, event, &mut drag, &mut menu);
            }
        }
        if let Some((seq, patch)) = state.ext.take_patch()
            && seq >= state.ext.scene_seq()
        {
            let next = patch.and_then(|patch| patch.accent);
            dirty |= accent != next;
            accent = next;
        }
        let current = state.version.load(Ordering::Acquire);
        let changed = current != version;
        if changed || dirty {
            version = current;
            if changed && !dirty {
                menu = None;
            }
            let session = state.active.load(Ordering::Relaxed);
            let snapshot = place::snapshot(state);
            if let Some((mut frame, snap)) = snapshot.as_ref().and_then(|snap| {
                let prefer_above = rendered
                    .as_ref()
                    .filter(|old| old.session == session)
                    .map(|old| old.above);
                Layout::new(
                    state,
                    session,
                    &snap.view,
                    menu,
                    accent,
                    snap.private,
                    prefer_above,
                )
                .map(|frame| (frame, snap))
            }) {
                hidden_at = None;
                if panel.is_none() {
                    let window = Window::new(layered)?;
                    let renderer = if layered {
                        Renderer::Solid(Surface::new()?)
                    } else {
                        Renderer::Composed(Box::new(Scene::new(window.0)?))
                    };
                    panel = Some((renderer, window));
                }
                let (scene, window) = panel.as_mut().unwrap();
                if let Some(old) = &rendered
                    && !changed
                {
                    frame.hover = old.hover;
                }
                let old_bounds = rendered
                    .as_ref()
                    .map(|old| motion.as_ref().map_or(old.rect, |m| m.window));
                if changed {
                    if let Some(old) = rendered.as_ref().filter(|_| !layered) {
                        let next = Motion::new(old, &frame, motion.as_ref());
                        if next.is_none()
                            && Motion::follows_strip_caret(old, &frame, motion.as_ref())
                            && let Some(active) = &mut motion
                        {
                            // A caret-only publication must not cancel an in-flight word fade.
                            active.window = frame.rect;
                        } else if next.is_some()
                            || old.rect != frame.rect
                            || old.session != frame.session
                            || old.private != frame.private
                            || !frame.effects
                            || old.menu != frame.menu
                            || old.spec.scroll != frame.spec.scroll
                        {
                            motion = next;
                        }
                    }
                }
                sample_light = frame.material.samples_ambient() && !snap.private;
                if !sample_light
                    || rendered
                        .as_ref()
                        .is_some_and(|old| old.session != frame.session)
                {
                    ambient = Ambient::default();
                }
                if sample_light && motion.is_none() {
                    ambient.refresh(frame.rect, old_bounds, frame.metrics.scale());
                }
                scene.paint(window.0, &frame, motion.as_ref(), ambient.light())?;
                if rendered.is_none() {
                    scene.enter(window.0, &frame, fading_until.take().is_some());
                }
                let bounds = motion.as_ref().map_or(frame.rect, |m| m.window);
                if old_bounds != Some(bounds)
                    || rendered
                        .as_ref()
                        .is_none_or(|old| old.metrics.scale() != frame.metrics.scale())
                {
                    show(window.0, bounds, frame.metrics.scale())?;
                }
                *state
                    .candidate_rect
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(frame.rect);
                if changed {
                    publish(state, Some((&frame, snap)));
                }
                rendered = Some(frame);
            } else if let Some(old) = rendered.take() {
                motion = None;
                ambient = Ambient::default();
                sample_light = false;
                if let Some((scene, window)) = &mut panel {
                    let same_session = state.active.load(Ordering::Relaxed) == old.session;
                    let ended = state
                        .active_published()
                        .is_some_and(|p| !p.host_draws && !p.composing);
                    if same_session && ended && old.effects && !old.private && scene.exit(window.0)
                    {
                        fading_until = Some(Instant::now() + Duration::from_millis(100));
                        fading_session = old.session;
                    } else {
                        let _ = unsafe { ShowWindow(window.0, SW_HIDE) };
                        scene.hide();
                    }
                }
                clear_candidate(state);
                publish(state, None);
                menu = None;
                drag = None;
                hidden_at = Some(Instant::now());
            }
        } else if let (Some(active), Some(frame), Some((scene, window))) =
            (&motion, &rendered, &mut panel)
        {
            if active.start.elapsed() >= active.duration {
                let resized = active.window != frame.rect;
                motion = None;
                scene.paint(window.0, frame, None, ambient.light())?;
                if resized {
                    show(window.0, frame.rect, frame.metrics.scale())?;
                }
            } else {
                scene.paint(window.0, frame, Some(active), ambient.light())?;
            }
        } else if sample_light
            && let (Some(frame), Some((scene, window))) = (&rendered, &mut panel)
            && ambient.refresh(frame.rect, None, frame.metrics.scale())
        {
            scene.paint(window.0, frame, None, ambient.light())?;
        }
        if fading_until.is_some()
            && (state.active.load(Ordering::Relaxed) != fading_session
                || state.active_published().is_none_or(|p| p.host_draws))
        {
            fading_until = Some(Instant::now());
        }
        if fading_until.is_some_and(|at| at <= Instant::now()) {
            if let Some((scene, window)) = &mut panel {
                let _ = unsafe { ShowWindow(window.0, SW_HIDE) };
                scene.hide();
            }
            fading_until = None;
        }
        if hidden_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(5)) {
            if panel.is_some() {
                drop(panel.take());
                unsafe { libmimalloc_sys::mi_collect(true) };
            }
            hidden_at = None;
        }
        let timeout = if motion.is_some() {
            Some(Duration::from_millis(16))
        } else {
            fading_until
                .map(|at| at.saturating_duration_since(Instant::now()))
                .or_else(|| {
                    ambient.timeout().or_else(|| {
                        hidden_at.map(|at| Duration::from_secs(5).saturating_sub(at.elapsed()))
                    })
                })
        };
        super::wait(Some(wake), timeout);
    }
}

pub(crate) fn run(state: Arc<State>) {
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if std::env::var_os("LUEYING_CANDIDATE_SOLID").is_none() {
        let queue: Result<DispatcherQueueController> = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })
        };
        match queue {
            Ok(_queue) => {
                if let Err(error) = event_loop(&state, false) {
                    println!("候选窗合成器不可用,改用实底: {error}");
                }
            }
            Err(error) => println!("候选窗消息队列不可用,改用实底: {error}"),
        }
    }
    clear_candidate(&state);
    publish(&state, None);
    if let Err(error) = event_loop(&state, true) {
        println!("实底候选窗不可用: {error}");
        clear_candidate(&state);
        publish(&state, None);
    }
    if init.is_ok() {
        unsafe { CoUninitialize() };
    }
}
