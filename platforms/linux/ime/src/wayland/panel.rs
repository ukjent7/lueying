//! 候选窗:软件绘制到 `wl_shm` 缓冲。
//!
//! 布局与绘制全部走 `lueying-render::panel`;这里提供 tiny-skia 画布与
//! 字体加载,并管理 `zwp_input_popup_surface_v2` 的缓冲生命周期。合成器
//! 负责把 popup 摆在文本输入框附近,我们只描述内容。

use std::os::fd::AsFd;
use std::path::Path;

use ab_glyph::{Font, FontArc, FontVec, PxScale, ScaleFont};
use lueying_protocol::SessionView;
use lueying_render::paint::{Canvas, Color, Point};
use lueying_render::panel::{
    self, Frame, Hit, Layout, Measure, Metrics, Palette, PanelSpec, Rect, Surface,
};
use memmap2::MmapMut;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect as SkRect, Stroke, Transform};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer, wl_shm, wl_surface};

use crate::wayland::app::App;
use crate::wayland::protocol::input_method::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2;

struct Buffer {
    _buffer: wl_buffer::WlBuffer,
    _map: MmapMut,
}

/// 候选窗对象,持有 popup surface 与当前缓冲。
pub struct Panel {
    surface: wl_surface::WlSurface,
    _popup: ZwpInputPopupSurfaceV2,
    shm: wl_shm::WlShm,
    font: Option<FontArc>,
    metrics: Metrics,
    palette: Palette,
    buffer: Option<Buffer>,
    spec: Option<PanelSpec>,
    hover: Option<usize>,
    dirty: bool,
    visible: bool,
}

impl Panel {
    pub fn new(
        surface: wl_surface::WlSurface,
        popup: ZwpInputPopupSurfaceV2,
        shm: wl_shm::WlShm,
        palette: Palette,
    ) -> Self {
        Self {
            surface,
            _popup: popup,
            shm,
            font: load_font(),
            metrics: Metrics::standard(),
            palette,
            buffer: None,
            spec: None,
            hover: None,
            dirty: false,
            visible: false,
        }
    }

    pub fn surface(&self) -> &wl_surface::WlSurface {
        &self.surface
    }

    /// 按会话快照重画;组合为空时隐藏 popup。
    pub fn draw(&mut self, view: &SessionView, qh: &QueueHandle<App>) {
        if view.raw.is_empty() {
            self.hide();
            return;
        }
        let mut spec = crate::ui::spec_of(view, Layout::Wrap);
        spec.width = crate::ui::measure_width(
            view,
            &self.metrics,
            &TextMeasure {
                font: self.font.as_ref(),
            },
        );
        if self.buffer.is_some() {
            self.spec = Some(spec);
            self.dirty = true;
            return;
        }
        let measure = TextMeasure {
            font: self.font.as_ref(),
        };
        let width = panel::panel_width(&spec, &self.metrics, &measure).ceil() as u32;
        let height = panel::panel_height(&spec, &self.metrics, &measure).ceil() as u32;
        let Some(mut pixmap) = Pixmap::new(width.max(1), height.max(1)) else {
            return;
        };
        {
            let mut canvas = TinyCanvas {
                pixmap: &mut pixmap,
                font: self.font.as_ref(),
            };
            let frame = Frame {
                spec: &spec,
                palette: &self.palette,
                metrics: self.metrics,
                surface: Surface::Solid,
                hover: self.hover.filter(|index| *index < spec.items.len()),
                pager: None,
            };
            panel::draw(&mut canvas, &frame, &measure);
        }
        if let Some(buffer) = self.upload(&pixmap, qh) {
            self.buffer = Some(buffer);
            self.spec = Some(spec);
            self.visible = true;
            self.dirty = false;
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        let Some(spec) = self.spec.as_ref() else {
            return Hit::Nothing;
        };
        panel::hit_test(
            spec,
            &self.metrics,
            &TextMeasure {
                font: self.font.as_ref(),
            },
            x,
            y,
        )
    }

    pub fn set_hover(&mut self, hover: Option<usize>) -> bool {
        if self.hover == hover {
            return false;
        }
        self.hover = hover;
        true
    }

    /// 缓冲被合成器释放;下一帧可以复用。
    pub fn release(&mut self) {
        self.buffer = None;
    }

    /// 取走「缓冲忙时积压的重画请求」。
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn hidden(&self) -> bool {
        !self.visible
    }

    pub fn hide(&mut self) {
        if self.visible {
            self.surface.attach(None, 0, 0);
            self.surface.commit();
            self.visible = false;
        }
        self.spec = None;
        self.hover = None;
    }

    fn upload(&self, pixmap: &Pixmap, qh: &QueueHandle<App>) -> Option<Buffer> {
        let width = pixmap.width();
        let height = pixmap.height();
        let size = width as usize * height as usize * 4;
        let mut file = memfd::MemfdOptions::default()
            .create("lueying-panel")
            .ok()?
            .into_file();
        file.set_len(size as u64).ok()?;
        let mut map = unsafe { MmapMut::map_mut(&file).ok()? };
        for (dst, src) in map.chunks_exact_mut(4).zip(pixmap.data().chunks_exact(4)) {
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
            (),
        );
        drop(pool);
        self.surface.attach(Some(&buffer), 0, 0);
        self.surface
            .damage_buffer(0, 0, width as i32, height as i32);
        self.surface.commit();
        Some(Buffer {
            _buffer: buffer,
            _map: map,
        })
    }
}

struct TextMeasure<'a> {
    font: Option<&'a FontArc>,
}

impl Measure for TextMeasure<'_> {
    fn width(&self, text: &str, size: f32) -> f32 {
        measure_text(self.font, text, size)
    }
}

struct TinyCanvas<'a> {
    pixmap: &'a mut Pixmap,
    font: Option<&'a FontArc>,
}

impl Canvas for TinyCanvas<'_> {
    fn fill_rect(&mut self, rect: Rect, color: Color) {
        let Some(rect) = sk_rect(rect) else {
            return;
        };
        let _ = self
            .pixmap
            .fill_rect(rect, &paint(color), Transform::identity(), None);
    }

    fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color) {
        let Some(rect) = sk_rect(rect) else {
            return;
        };
        let mut pb = PathBuilder::new();
        pb.push_rect(rect);
        let Some(path) = pb.finish() else {
            return;
        };
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        let _ = self
            .pixmap
            .stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
    }

    fn fill_rrect(&mut self, rect: Rect, radius: f32, color: Color) {
        if let Some(path) = rrect(rect, radius) {
            let _ = self.pixmap.fill_path(
                &path,
                &paint(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn stroke_rrect(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        if let Some(path) = rrect(rect, radius) {
            let stroke = Stroke {
                width,
                ..Stroke::default()
            };
            let _ =
                self.pixmap
                    .stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
        }
    }

    fn shadow_rrect(&mut self, rect: Rect, radius: f32, blur: f32, color: Color) {
        // tiny-skia 没有模糊蒙版;用几层向外扩张、逐渐变淡的圆角矩形叠出
        // 柔和边缘。层数固定,每帧成本可控。
        const LAYERS: u32 = 6;
        for index in 0..LAYERS {
            let grow = blur * (index as f32 + 1.0) / LAYERS as f32;
            let alpha = color.alpha / (LAYERS as u8 * 2);
            let layer = Rect::from_ltrb(
                rect.left - grow,
                rect.top - grow + blur * 0.35,
                rect.right + grow,
                rect.bottom + grow + blur * 0.35,
            );
            self.fill_rrect(layer, radius + grow, Color::with_alpha(color.rgb, alpha));
        }
    }

    fn text(
        &mut self,
        content: &str,
        origin: Point,
        size: f32,
        color: Color,
        max_width: Option<f32>,
    ) {
        let Some(font) = self.font.cloned() else {
            return;
        };
        let scaled = font.as_scaled(PxScale::from(size));
        let mut caret = origin.x;
        let mut previous = None;
        for ch in content.chars() {
            let glyph = scaled.glyph_id(ch);
            if let Some(last) = previous {
                caret += scaled.kern(last, glyph);
            }
            if let Some(max) = max_width
                && caret - origin.x >= max
            {
                break;
            }
            let glyph =
                glyph.with_scale_and_position(PxScale::from(size), ab_glyph::point(caret, 0.0));
            let id = glyph.id;
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                let baseline = origin.y + size * 0.36;
                outline.draw(|x, y, coverage| {
                    let px = bounds.min.x + x as f32;
                    let py = baseline + bounds.min.y + y as f32;
                    self.blend(px, py, color, coverage);
                });
            }
            caret += scaled.h_advance(id);
            previous = Some(id);
        }
    }

    fn measure(&mut self, content: &str, size: f32) -> f32 {
        measure_text(self.font, content, size)
    }

    fn text_centered(&mut self, content: &str, rect: Rect, size: f32, color: Color) {
        let width = measure_text(self.font, content, size);
        let origin = Point::new(
            rect.left + (rect.width() - width) * 0.5,
            (rect.top + rect.bottom) * 0.5,
        );
        self.text(content, origin, size, color, None);
    }
}

impl TinyCanvas<'_> {
    fn blend(&mut self, x: f32, y: f32, color: Color, coverage: f32) {
        if x < 0.0 || y < 0.0 {
            return;
        }
        let (x, y) = (x as u32, y as u32);
        if x >= self.pixmap.width() || y >= self.pixmap.height() {
            return;
        }
        let index = ((y * self.pixmap.width() + x) * 4) as usize;
        let alpha = coverage * (color.alpha as f32 / 255.0);
        let inv = 1.0 - alpha;
        let red = ((color.rgb >> 16) & 0xff) as f32;
        let green = ((color.rgb >> 8) & 0xff) as f32;
        let blue = (color.rgb & 0xff) as f32;
        let data = self.pixmap.data_mut();
        data[index] = (red * alpha + data[index] as f32 * inv) as u8;
        data[index + 1] = (green * alpha + data[index + 1] as f32 * inv) as u8;
        data[index + 2] = (blue * alpha + data[index + 2] as f32 * inv) as u8;
        data[index + 3] = (alpha * 255.0 + data[index + 3] as f32 * inv) as u8;
    }
}

fn paint(color: Color) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        ((color.rgb >> 16) & 0xff) as u8,
        ((color.rgb >> 8) & 0xff) as u8,
        (color.rgb & 0xff) as u8,
        color.alpha,
    );
    paint.anti_alias = true;
    paint
}

fn sk_rect(rect: Rect) -> Option<SkRect> {
    SkRect::from_ltrb(rect.left, rect.top, rect.right, rect.bottom)
}

fn rrect(rect: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let radius = radius
        .max(0.0)
        .min(rect.width() * 0.5)
        .min(rect.height() * 0.5);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let mut pb = PathBuilder::new();
    pb.move_to(rect.left + radius, rect.top);
    pb.line_to(rect.right - radius, rect.top);
    pb.quad_to(rect.right, rect.top, rect.right, rect.top + radius);
    pb.line_to(rect.right, rect.bottom - radius);
    pb.quad_to(rect.right, rect.bottom, rect.right - radius, rect.bottom);
    pb.line_to(rect.left + radius, rect.bottom);
    pb.quad_to(rect.left, rect.bottom, rect.left, rect.bottom - radius);
    pb.line_to(rect.left, rect.top + radius);
    pb.quad_to(rect.left, rect.top, rect.left + radius, rect.top);
    pb.close();
    pb.finish()
}

fn measure_text(font: Option<&FontArc>, text: &str, size: f32) -> f32 {
    let Some(font) = font else {
        return text.chars().count() as f32 * size * 0.5;
    };
    let scaled = font.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut previous = None;
    for ch in text.chars() {
        let glyph = scaled.glyph_id(ch);
        if let Some(last) = previous {
            width += scaled.kern(last, glyph);
        }
        width += scaled.h_advance(glyph);
        previous = Some(glyph);
    }
    width
}

/// 按常见中文字体优先级挑选;找不到时退回系统默认无衬线。
fn load_font() -> Option<FontArc> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    const FAMILIES: [&str; 6] = [
        "Noto Sans CJK SC",
        "Source Han Sans SC",
        "Noto Sans SC",
        "WenQuanYi Micro Hei",
        "Microsoft YaHei",
        "DejaVu Sans",
    ];
    let families: Vec<fontdb::Family<'_>> = FAMILIES
        .iter()
        .map(|name| fontdb::Family::Name(*name))
        .collect();
    let query = fontdb::Query {
        families: &families,
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        style: fontdb::Style::Normal,
    };
    let id = db.query(&query).or_else(|| {
        db.query(&fontdb::Query {
            families: &[fontdb::Family::SansSerif],
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        })
    })?;
    db.with_face_data(id, |data, index| {
        FontVec::try_from_vec_and_index(data.to_vec(), index)
            .ok()
            .map(FontArc::from)
    })
    .flatten()
    .or_else(load_fallback_file)
}

fn load_fallback_file() -> Option<FontArc> {
    const PATHS: [&str; 3] = [
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    PATHS
        .iter()
        .find_map(|path| std::fs::read(Path::new(path)).ok())
        .and_then(|data| FontVec::try_from_vec_and_index(data, 0).ok())
        .map(FontArc::from)
}
