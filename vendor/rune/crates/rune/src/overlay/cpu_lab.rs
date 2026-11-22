use super::{Backing, OverlayApp, OverlayCore, RuntimeCore, frame, presence};
use crate::platform::win32::{dib::Dib, hwnd};
use crate::render::cpu_boot::{self, CpuPainter, Scene, Target};
use crate::time::Instant;
use crate::window::{BackoffEvent, TIMER_FRAME};
use std::sync::OnceLock;
use std::time::Duration;
use winit::{event_loop::ActiveEventLoop, monitor::MonitorHandle, window::Window};

pub(super) struct Presenter {
    pub(super) a11y: Option<crate::a11y::Adapter>,
    painter: CpuPainter,
    buffer: Option<(Dib, (u32, u32))>,
    monitor: Option<MonitorHandle>,
    period: Duration,
    refresh_check: Instant,
    trace: Option<Instant>,
}

impl Presenter {
    pub(super) fn new() -> Self {
        let now = crate::time::now();
        Self {
            a11y: None, painter: CpuPainter::new(2).with_backdrop_cache(), buffer: None, monitor: None,
            period: TIMER_FRAME, refresh_check: now,
            trace: std::env::var_os("RUNE_LAB_CPU_TRACE").map(|_| now),
        }
    }

    /// Frees the cached blur payload: a hide no longer needs the blur it kept.
    pub(super) fn clear_backdrop_cache(&mut self) {
        self.painter.clear_backdrop_cache();
    }

    pub(super) fn detach(&mut self) {
        self.a11y.take();
        self.buffer.take();
        self.painter.forget();
        self.monitor.take();
    }

    fn period(&mut self, window: &Window, now: Instant) -> Duration {
        let monitor = window.current_monitor();
        if monitor != self.monitor || now >= self.refresh_check {
            self.period = monitor.as_ref().and_then(MonitorHandle::refresh_rate_millihertz)
                .filter(|&mhz| mhz >= 24_000)
                .map_or(TIMER_FRAME, |mhz| Duration::from_nanos(1_000_000_000_000 / u64::from(mhz)));
            self.monitor = monitor;
            self.refresh_check = now + Duration::from_secs(2);
        }
        self.period
    }

    fn paint(&mut self, app: &mut dyn OverlayApp, window: &Window, size: (u32, u32), force: bool) -> Result<bool, String> {
        let has_compositor = app.has_compositor();
        let rt = app.rt_mut();
        self.painter.fill_masks(&mut rt.text.atlas);
        let scene = scene(rt, size, has_compositor);
        if let Some(reason) = scene.needs_gpu() {
            return Err(format!("CPU overlay cannot draw these instances: {reason:?}"));
        }
        if !self.buffer.as_ref().is_some_and(|(dib, _)| dib.fits(size)) {
            let dib = Dib::new(size.0, size.1).ok_or("could not allocate CPU overlay DIB")?;
            self.painter.forget();
            self.buffer = Some((dib, size));
        }
        let (dib, last_size) = self.buffer.as_mut().unwrap();
        let stride = dib.stride();
        let painted = self.painter.paint(&scene, &mut Target::with_stride(dib.bytes(), size.0, size.1, stride))
            .map_err(|reason| format!("CPU overlay paint: {reason:?}"))?;
        let submit = force || size != *last_size || !painted.rects.is_empty();
        if submit {
            dib.present_layered(hwnd(window).ok_or("CPU overlay HWND missing")?, size, 255)
                .map_err(|err| format!("CPU overlay UpdateLayeredWindow: {err}"))?;
            *last_size = size;
        }
        for page in &mut rt.images.pages { page.dirty = None; }
        Ok(submit)
    }

    pub(super) fn fade(&mut self, window: &Window, alpha: u8) {
        if let (Some(handle), Some((dib, size))) = (hwnd(window), &self.buffer) {
            let _ = dib.present_layered(handle, *size, alpha);
        }
    }
}

impl OverlayCore {
    pub(super) fn enable_cpu(&mut self, rt: &mut RuntimeCore) {
        cpu_boot::attach_text(&mut rt.text.atlas);
        self.cpu = Some(Presenter::new());
    }

    pub(super) fn show_cpu(&mut self, app: &mut dyn OverlayApp, event_loop: &ActiveEventLoop, fade: Duration) {
        let fading = self.presence.fading();
        self.resolve_size(event_loop);
        self.prepare(app, event_loop, true);
        let Some(Backing::Window(window)) = self.surface.parked() else { return };
        let window = window.clone();
        self.resume_live(app);
        self.resized(app, event_loop);
        if !fading { self.settle_before_reveal(app.rt_mut()); }
        self.backoff_step(BackoffEvent::Revealed);
        if !self.paint_cpu(app, &window, true) { return; }
        self.show_window(&window, fade);
        if self.shared.failure.lock().unwrap().is_some() { return; }
        let effects = self.presence.step(presence::Event::Show, self.release);
        self.carry_out(app, effects);
    }

    pub(super) fn draw_cpu(&mut self, app: &mut dyn OverlayApp) {
        self.apply_input(app.rt());
        if self.idle && !self.dirty || self.backing_off(crate::time::now()) { return; }
        let Some(Backing::Window(window)) = self.surface.parked() else { return };
        let window = window.clone();
        if !self.paint_cpu(app, &window, false) {
            self.hide(app);
        }
    }

    fn paint_cpu(&mut self, app: &mut dyn OverlayApp, window: &Window, force: bool) -> bool {
        let now = crate::time::now();
        let scale = crate::window::backing_scale(window);
        let size = window.inner_size();
        let size = (size.width.max(1), size.height.max(1));
        let rt = app.rt_mut();
        rt.transparent = true;
        rt.set_viewport((size.0 as f32 / scale, size.1 as f32 / scale), scale);
        let (animating, _) = frame::step(app, &mut self.clock, now, false);
        super::input::drop_window_requests(app.rt_mut());
        self.clipboard.flush_core(app.rt_mut());
        if let Some(native) = &self.native { native.set_regions(app.rt().hit_regions(), scale); }
        self.apply_cursor(app.rt());
        let cpu = self.cpu.as_mut().unwrap();
        let period = cpu.period(window, now);
        let submitted = match cpu.paint(app, window, size, force) {
            Ok(submitted) => submitted,
            Err(reason) => {
                self.fail(reason);
                self.idle = true;
                self.dirty = false;
                return false;
            }
        };
        let done = crate::time::now();
        let work = done.saturating_duration_since(now);
        if let Some(start) = cpu.trace {
            eprintln!("rune-cpu-pace,{},{},{},{},{},{:.2},{}",
                done.saturating_duration_since(start).as_micros(), work.as_micros(), period.as_micros(),
                size.0, size.1, scale, u8::from(submitted));
        }
        self.clock.presented_core(app.rt_mut(), done);
        self.dirty = false;
        self.idle = frame::idle_after(app.rt(), animating, None);
        self.runtime_idle = self.idle;
        if !self.idle {
            // Skip slots already consumed by rendering instead of queueing catch-up frames.
            let slots = (work.as_nanos() / period.as_nanos() + 1) as u32;
            self.backoff_step(BackoffEvent::At(now + period * slots));
        }
        true
    }
}

pub(super) fn scene(rt: &RuntimeCore, size: (u32, u32), compositor: bool) -> Scene<'_> {
    Scene::new(&rt.instances, &rt.text.atlas, &rt.images, size, [0.0; 4])
        .srgb_blend(rt.srgb_blend())
        .native_surfaces(!rt.surfaces.is_empty())
        .compositor(compositor)
        .font_smoothing(rt.font_smoothing())
}

pub(super) fn audit(app: &dyn OverlayApp, size: (u32, u32)) {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("RUNE_LAB_CPU_AUDIT").is_some()) { return; }
    let rt = app.rt();
    let scene = scene(rt, size, app.has_compositor());
    let backdrops = rt.instances.iter().filter(|i| i.kind() == Some(crate::render::Kind::BackdropBlur)).count();
    eprintln!("rune-cpu-audit,{},{},{},{},{:?}", size.0, size.1, rt.instances.len(), backdrops, scene.needs_gpu());
}
