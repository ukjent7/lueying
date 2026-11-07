//! 工具栏的 Wayland 版:layer-shell 窗口,锚在屏幕右下角,按钮与角色由 `lueying-render` 描述,
//! 用 tiny-skia 软件绘制到 shm 缓冲。
//!
//! layer-shell 不让客户端决定左上角:窗口锚在右下角,位置由右边距与下边距给出,拖动就是改这两个边距。
//! 合成器不支持 layer-shell(例如 GNOME)时工具栏不出现,输入法照常工作。

use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use ab_glyph::FontArc;
use lueying_render::panel::{Palette, Rect};
use lueying_render::skin_pack::{self, SkinPack};
use lueying_render::toolbar::{self as model, Button, Players, Scene, Status, paint};
use memmap2::MmapMut;
use tiny_skia::Pixmap;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_shm, wl_surface};
use wayland_client::{Proxy, QueueHandle};

use crate::config;
use crate::wayland::app::{App, Owner};
use crate::wayland::panel::{TinyCanvas, load_font};
use crate::wayland::protocol::layer_shell::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use crate::wayland::protocol::layer_shell::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;

/// 逐帧的间隔:有角色在动时按这个节拍重画。
pub const FRAME: Duration = Duration::from_millis(33);
/// layer-shell 的层级:顶层。
const LAYER_TOP: u32 = 2;
/// 锚点的位掩码:下 2、右 8。
const ANCHOR_BOTTOM_RIGHT: u32 = 2 | 8;

/// 工具栏启动时的状态,会话同步过来之前用它。
const INITIAL: Status = Status {
    chinese: true,
    fullwidth: false,
    punct: true,
    traditional: false,
};

struct Buffer {
    _buffer: wl_buffer::WlBuffer,
    _map: MmapMut,
}

/// 工具栏窗口:持有 layer surface 与当前缓冲;按钮、位置与角色都在 `lueying-render`。
pub struct Toolbar {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    shm: wl_shm::WlShm,
    font: Option<FontArc>,
    palette: Palette,
    /// 屏幕大小(逻辑像素),左上角为原点;位置都换算到它上面。
    screen: Rect,
    bar: model::Toolbar,
    status: Status,
    skin: Option<SkinPack>,
    sprites: Vec<Pixmap>,
    players: Players,
    started: Instant,
    now_ms: u64,
    /// 左键按着;按下时落在哪个按钮上;按下后是否真的拖动过。
    held: bool,
    pressed: Option<Button>,
    moved: bool,
    /// 收起后不映射;下一次激活时重新提交,合成器会再配置一次。
    collapsed: bool,
    buffer: Option<Buffer>,
    dirty: bool,
}

impl Toolbar {
    /// 建窗口并提交一次,等合成器的 configure 再画第一帧。
    pub fn new(
        compositor: &wl_compositor::WlCompositor,
        shell: &ZwlrLayerShellV1,
        shm: wl_shm::WlShm,
        screen: Rect,
        qh: &QueueHandle<App>,
    ) -> Self {
        let surface = compositor.create_surface(qh, ());
        let layer = shell.get_layer_surface(
            &surface,
            None,
            LAYER_TOP,
            "lueying-toolbar".to_string(),
            qh,
            (),
        );
        let (skin, sprites) = load_pack();
        let bar = model::Toolbar::at_default(screen);
        let players = Players::new(skin.as_ref(), &bar, screen);
        let toolbar = Self {
            surface,
            layer,
            shm,
            font: load_font(),
            palette: crate::theme::current(),
            screen,
            bar,
            status: INITIAL,
            skin,
            sprites,
            players,
            started: Instant::now(),
            now_ms: 0,
            held: false,
            pressed: None,
            moved: false,
            collapsed: false,
            buffer: None,
            dirty: false,
        };
        toolbar.place();
        toolbar
    }

    /// 这个 surface 是不是工具栏的。
    pub fn is(&self, surface: &wl_surface::WlSurface) -> bool {
        self.surface.id() == surface.id()
    }

    /// 合成器给出了窗口尺寸:确认配置,然后画一帧。
    pub fn configure(&mut self, serial: u32, qh: &QueueHandle<App>) {
        self.layer.ack_configure(serial);
        self.redraw(qh);
    }

    /// 推进时间与角色;有角色在动时返回 true,调用方随即重画。
    pub fn tick(&mut self) -> bool {
        self.now_ms = self.now();
        self.players.advance(&self.bar, self.screen, self.now_ms)
    }

    /// 按键:角色收到一次按键。
    pub fn key_pressed(&mut self) {
        let now = self.now();
        self.players.key(now);
    }

    /// 按钮上的字随状态变;变了就重画。
    pub fn set_status(&mut self, status: Status, qh: &QueueHandle<App>) {
        if self.status != status {
            self.status = status;
            self.redraw(qh);
        }
    }

    /// 画一帧并交给合成器。上一帧还没被释放时只记下「要重画」,释放时再画。
    pub fn redraw(&mut self, qh: &QueueHandle<App>) {
        if self.collapsed {
            return;
        }
        if self.buffer.is_some() {
            self.dirty = true;
            return;
        }
        let stage = self.bar.stage(self.screen);
        let width = stage.width().ceil().max(1.0) as u32;
        let height = stage.height().ceil().max(1.0) as u32;
        let Some(mut pixmap) = Pixmap::new(width, height) else {
            return;
        };
        {
            let mut canvas = TinyCanvas {
                pixmap: &mut pixmap,
                font: self.font.as_ref(),
                sprites: &self.sprites,
            };
            let scene = Scene {
                toolbar: &self.bar,
                origin: (stage.left, stage.top),
                palette: &self.palette,
                status: self.status,
                pack: self.skin.as_ref().map(|skin| &skin.pack),
                now_ms: self.now_ms,
                below: self.players.below_cast(),
                above: self.players.above_cast(),
            };
            paint(&mut canvas, &scene);
        }
        self.upload(&pixmap, qh);
    }

    /// 合成器用完了缓冲:丢掉它。
    pub fn buffer_released(&mut self) {
        self.buffer = None;
    }

    /// 取走「缓冲忙时积压的重画请求」。
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// 左键按下:记下落在哪个按钮上,以及拖动的起点。
    pub fn press(&mut self, pos: (f32, f32)) {
        let (x, y) = self.screen_pos(pos);
        self.held = true;
        self.moved = false;
        self.pressed = self.bar.hit(x, y);
        self.bar.press(x, y);
    }

    /// 指针移动:按着就拖动,位置变了就改边距。
    pub fn motion(&mut self, pos: (f32, f32)) {
        if !self.held {
            return;
        }
        let (x, y) = self.screen_pos(pos);
        let before = (self.bar.x, self.bar.y);
        self.bar.drag(x, y, self.screen);
        if (self.bar.x, self.bar.y) != before {
            self.moved = true;
            self.place();
        }
    }

    /// 左键松开:没有拖动就是点击。点在同一个按钮上返回按钮;点在按钮以外就是点角色。
    pub fn release(&mut self, pos: (f32, f32)) -> Option<Button> {
        let (x, y) = self.screen_pos(pos);
        self.held = false;
        self.bar.release();
        let pressed = self.pressed.take();
        if std::mem::take(&mut self.moved) {
            return None;
        }
        match (pressed, self.bar.hit(x, y)) {
            (Some(p), Some(r)) if p == r => Some(p),
            (None, _) => {
                self.players.click(x, y);
                None
            }
            _ => None,
        }
    }

    /// 收起:卸下缓冲并解除映射。下一次激活时再提交一次,合成器会重新配置窗口。
    pub fn collapse(&mut self) {
        self.collapsed = true;
        self.buffer = None;
        self.surface.attach(None, 0, 0);
        self.surface.commit();
    }

    /// 激活时:如果之前收起过,就重新提交,等配置后再出现。
    pub fn expand(&mut self) {
        if std::mem::take(&mut self.collapsed) {
            self.surface.commit();
        }
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// 窗口内的坐标换成屏幕坐标(模型用屏幕坐标)。
    fn screen_pos(&self, pos: (f32, f32)) -> (f32, f32) {
        let stage = self.bar.stage(self.screen);
        (pos.0 + stage.left, pos.1 + stage.top)
    }

    /// 按模型摆窗口:锚在右下角,边距就是舞台离屏幕右下角的距离。
    fn place(&self) {
        let stage = self.bar.stage(self.screen);
        self.layer.set_size(
            stage.width().ceil() as u32,
            stage.height().ceil() as u32,
        );
        self.layer.set_anchor(ANCHOR_BOTTOM_RIGHT);
        self.layer.set_margin(
            0,
            (self.screen.right - stage.right).round() as i32,
            (self.screen.bottom - stage.bottom).round() as i32,
            0,
        );
        self.layer.set_keyboard_interactivity(0);
        self.surface.commit();
    }

    fn upload(&mut self, pixmap: &Pixmap, qh: &QueueHandle<App>) {
        let (width, height) = (pixmap.width(), pixmap.height());
        let size = width as usize * height as usize * 4;
        let Ok(memfd) = memfd::MemfdOptions::default().create("lueying-toolbar") else {
            return;
        };
        let file = memfd.into_file();
        if file.set_len(size as u64).is_err() {
            return;
        }
        let Ok(mut map) = (unsafe { MmapMut::map_mut(&file) }) else {
            return;
        };
        for (dst, src) in map
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(pixmap.data().as_chunks::<4>().0.iter())
        {
            dst.copy_from_slice(&[src[2], src[1], src[0], src[3]]);
        }
        let pool = self.shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            width as i32 * 4,
            wl_shm::Format::Argb8888,
            qh,
            Owner::Toolbar,
        );
        drop(pool);
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface
            .damage_buffer(0, 0, width as i32, height as i32);
        self.surface.commit();
        self.buffer = Some(Buffer {
            _buffer: buffer,
            _map: map,
        });
        self.dirty = false;
    }
}

/// 读动画包:`pack.json` 与帧图都在 `toolbar-pack` 里。缺了或坏了就不画角色,按钮照常。
fn load_pack() -> (Option<SkinPack>, Vec<Pixmap>) {
    let Some(dir) = config::toolbar_pack_dir() else {
        return (None, Vec::new());
    };
    let Ok(text) = std::fs::read_to_string(dir.join("pack.json")) else {
        return (None, Vec::new());
    };
    let skin = match skin_pack::parse(&text) {
        Ok(skin) => skin,
        Err(e) => {
            eprintln!("工具栏动画包错误: {e}");
            return (None, Vec::new());
        }
    };
    let Some(last) = skin.pack.max_frame() else {
        return (None, Vec::new());
    };
    let frames = dir.join(&skin.frames);
    let mut sprites = Vec::with_capacity(last + 1);
    for number in 0..=last {
        match Pixmap::load_png(frames.join(format!("{number}.png"))) {
            Ok(image) => sprites.push(image),
            Err(e) => {
                eprintln!("工具栏动画帧 {number} 读取失败: {e}");
                return (None, Vec::new());
            }
        }
    }
    (Some(skin), sprites)
}
