//! 候选窗专用材质参数;环境色只照亮釉面,不改变控件的透明度。

use lueying_render::paint::{Canvas, Color, Point};
use lueying_render::panel::{Metrics, PANEL_RADIUS, Palette, Rect, Surface};
use lueying_server::config::UiMaterial;

use super::candidate_light::Light;

#[derive(Clone, Copy)]
pub(super) enum Material {
    Plain,
    BlackPorcelain,
    Glass(f32),
}

impl Material {
    pub(super) fn samples_ambient(self) -> bool {
        matches!(self, Self::BlackPorcelain)
    }

    pub(super) fn new(material: UiMaterial, frost: f32) -> Self {
        match material {
            UiMaterial::Glass => Self::Glass(if frost.is_finite() {
                frost.clamp(0.0, 1.0)
            } else {
                0.0
            }),
            UiMaterial::Ceramic => Self::BlackPorcelain,
        }
    }

    pub(super) fn tint(self, backdrop: bool) -> u8 {
        match self {
            Self::Glass(frost) if backdrop => blend(46, 201, frost),
            _ => 0,
        }
    }

    pub(super) fn palette(self, palette: &mut Palette, accent_override: bool) {
        if !matches!(self, Self::BlackPorcelain) {
            return;
        }
        palette.bg = 0x111719;
        palette.surface_alt = 0x20292c;
        palette.border = 0x485759;
        palette.text = 0xf4f8f7;
        palette.text_secondary = 0xd4dfde;
        palette.text_tertiary = 0xb2c2c1;
        palette.hover = 0x304044;
        if !accent_override {
            palette.selected_bg = 0xc4dcd7;
            palette.selected_text = 0x142e2b;
        }
    }

    pub(super) fn underlay(
        self,
        canvas: &mut dyn Canvas,
        metrics: &Metrics,
        width: i32,
        height: f32,
        backdrop: bool,
        light: Light,
        flex: f32,
    ) -> Surface {
        let s = metrics.scale();
        let (w, h) = (width as f32, height);
        let face = Rect::from_xywh(0.5 * s, 0.5 * s, w - s, h - s);
        let inset = |by: f32| {
            Rect::from_xywh(
                by * s,
                by * s,
                (w - 2.0 * by * s).max(1.0),
                (h - 2.0 * by * s).max(1.0),
            )
        };
        let radius = PANEL_RADIUS * s;
        match self {
            Self::BlackPorcelain => {
                black_porcelain(canvas, face, inset(1.6), radius, s, light);
                Surface::Painted
            }
            Self::Glass(frost) if backdrop => {
                // Clear glass concentrates light at the rim. Frost spreads the veil over the face.
                let flare = (flex * (1.0 - 0.72 * frost) * 45.0).round() as u8;
                canvas.fill_rrect_vertical(
                    inset(1.0),
                    radius - s,
                    Color::with_alpha(0xfff8e6, blend(10, 26, frost)),
                    Color::with_alpha(0x100c08, blend(52, 24, frost)),
                );
                canvas.stroke_rrect(
                    face,
                    radius,
                    s,
                    Color::with_alpha(0x170f0b, blend(124, 80, frost)),
                );
                canvas.stroke_rrect(
                    inset(1.5),
                    radius - s,
                    s,
                    Color::with_alpha(0xfffbec, blend(108, 42, frost) + flare),
                );
                Surface::Glass
            }
            _ => Surface::Solid,
        }
    }
}

fn reflection(rect: Rect, light: Light) -> Option<(Point, f32)> {
    let axis = light.x.abs().max(light.y.abs());
    if axis < 0.05 {
        return None;
    }
    let focus = ((axis - 0.05) / 0.32).clamp(0.0, 1.0);
    let reach = (axis / 0.32).clamp(0.0, 1.0);
    Some((
        Point::new(
            rect.center_x() + light.x / axis * rect.width() * 0.46 * reach,
            rect.center_y() + light.y / axis * rect.height() * 0.46 * reach,
        ),
        focus,
    ))
}

fn black_porcelain(
    canvas: &mut dyn Canvas,
    outer: Rect,
    body: Rect,
    radius: f32,
    s: f32,
    light: Light,
) {
    let body_radius = (radius - 1.1 * s).max(0.0);
    let reflection = reflection(outer, light);
    canvas.fill_rrect(outer, radius, Color::opaque(0x34474c));
    let glint = light.tint(0xffffff, 0.64);
    // Both highlights share the same measured source. The vertical footprint grows more
    // slowly than the panel, so the moving spring does not simply stretch the reflection.
    let depth = (outer.height() - 37.0 * s).max(0.0);
    let bend = depth / (depth + 78.0 * s);
    let extent = Point::new(
        outer.width() * (0.40 + 0.06 * bend) * 0.83,
        (44.0 + 44.0 * bend) * s * 0.83,
    );
    let halo = light.tint(0xaabdc8, 0.67);
    let edge_alpha = |focus: f32| ((176.0 + 79.0 * light.strength) * focus).round() as u8;
    if let Some((center, focus)) = reflection {
        let alpha = edge_alpha(focus);
        canvas.light_rrect(
            outer,
            radius,
            center,
            Point::new(extent.x * 0.86, extent.y * 0.67),
            &[
                (0.0, Color::with_alpha(glint, alpha)),
                (0.44, Color::with_alpha(glint, alpha - alpha / 6)),
                (1.0, Color::with_alpha(glint, 0)),
            ],
        );
    }
    canvas.fill_rrect_vertical(
        body,
        body_radius,
        Color::opaque(0x101517),
        Color::opaque(0x0d1416),
    );
    let Some((center, focus)) = reflection else {
        return;
    };
    let strength = (136.0 * (0.45 + 0.63 * light.strength) * focus)
        .round()
        .min(255.0) as u8;
    canvas.light_rrect(
        body,
        body_radius,
        center,
        extent,
        &[
            (0.0, Color::with_alpha(halo, strength)),
            (0.44, Color::with_alpha(halo, strength - strength / 4)),
            (0.72, Color::with_alpha(halo, strength / 5)),
            (1.0, Color::with_alpha(halo, 0)),
        ],
    );
}

fn blend(clear: u8, frosted: u8, frost: f32) -> u8 {
    (f32::from(clear) + (f32::from(frosted) - f32::from(clear)) * frost).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflection_reaches_each_lit_edge_without_a_default_bright_corner() {
        let panel = Rect::from_xywh(4.0, 6.0, 480.0, 40.0);
        let mut light = Light::default();
        assert!(reflection(panel, light).is_none());
        for (x, y) in [(-0.5, 0.0), (0.5, 0.0), (0.0, -0.5), (0.0, 0.5)] {
            light.x = x;
            light.y = y;
            let (center, focus) = reflection(panel, light).unwrap();
            assert!((center.x - (panel.center_x() + x * 441.6)).abs() < 0.01);
            assert!((center.y - (panel.center_y() + y * 36.8)).abs() < 0.01);
            assert_eq!(focus, 1.0);
        }
    }

    #[test]
    fn glass_tint_is_continuous_and_does_not_leak_into_solid_fallback() {
        let tints: Vec<_> = (0..=100)
            .map(|step| Material::new(UiMaterial::Glass, step as f32 / 100.0).tint(true))
            .collect();
        assert_eq!((tints[0], tints[100]), (46, 201));
        assert!(
            tints
                .windows(2)
                .all(|pair| pair[0] <= pair[1] && pair[1] - pair[0] <= 2)
        );
        assert_eq!(Material::Glass(0.5).tint(false), 0);
    }
}
