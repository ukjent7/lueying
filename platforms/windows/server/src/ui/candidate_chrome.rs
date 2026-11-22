//! Retained vector chrome. This does not allocate a separate drawing surface.

use lueying_render::panel::{self, Rect};
use windows::{
    Foundation::{IReference, PropertyValue, TimeSpan},
    UI::{Color, Composition::*},
    core::{Interface, Result, h},
};
use windows_numerics::{Vector2, Vector3};

fn color(rgb: u32, alpha: u8) -> Color {
    Color {
        A: alpha,
        R: (rgb >> 16) as u8,
        G: (rgb >> 8) as u8,
        B: rgb as u8,
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct RailPose {
    pub(super) cap: Rect,
    pub(super) rail: Option<Rect>,
    pub(super) scroll: Option<(Rect, Rect)>,
    pub(super) ink: u32,
    pub(super) border: u32,
    pub(super) hover_cap: bool,
    pub(super) hover_scroll: bool,
    pub(super) expanded: bool,
    pub(super) glass: bool,
}

struct RoundedShape {
    geometry: CompositionRoundedRectangleGeometry,
    brush: CompositionColorBrush,
}

impl RoundedShape {
    fn append(compositor: &Compositor, shapes: &CompositionShapeCollection) -> Result<Self> {
        let geometry = compositor.CreateRoundedRectangleGeometry()?;
        let brush = compositor.CreateColorBrush()?;
        let shape = compositor.CreateSpriteShapeWithGeometry(&geometry)?;
        shape.SetFillBrush(&brush)?;
        shapes.Append(&shape)?;
        Ok(Self { geometry, brush })
    }

    fn place(&self, rect: Rect, fill: Color) -> Result<()> {
        let width = rect.width();
        self.geometry.SetOffset(Vector2 {
            X: rect.left,
            Y: rect.top,
        })?;
        self.geometry.SetSize(Vector2 {
            X: width,
            Y: rect.height(),
        })?;
        self.geometry.SetCornerRadius(Vector2 {
            X: width * 0.5,
            Y: width * 0.5,
        })?;
        self.brush.SetColor(fill)
    }
}

pub(super) struct Rail {
    clip: ContainerVisual,
    visual: ShapeVisual,
    // This shape also clips the track and thumb, so the controls cannot lead the rail.
    shell: RoundedShape,
    track: RoundedShape,
    thumb: RoundedShape,
    stretch: SpringScalarNaturalMotionAnimation,
    last: Option<(RailPose, f32, bool)>,
}

impl Rail {
    pub(super) fn new(compositor: &Compositor, children: &VisualCollection) -> Result<Self> {
        let clip = compositor.CreateContainerVisual()?;
        clip.SetIsVisible(false)?;
        let visual = compositor.CreateShapeVisual()?;
        let shapes = visual.Shapes()?;
        let shell = RoundedShape::append(compositor, &shapes)?;
        let track = RoundedShape::append(compositor, &shapes)?;
        let thumb = RoundedShape::append(compositor, &shapes)?;
        clip.SetClip(&compositor.CreateGeometricClipWithGeometry(&shell.geometry)?)?;
        clip.Children()?.InsertAtTop(&visual)?;
        let stretch = compositor.CreateSpringScalarAnimation()?;
        stretch.SetDampingRatio(0.78)?;
        stretch.SetPeriod(TimeSpan {
            Duration: 1_450_000,
        })?;
        children.InsertAtTop(&clip)?;
        Ok(Self {
            clip,
            visual,
            shell,
            track,
            thumb,
            stretch,
            last: None,
        })
    }

    pub(super) fn set(&mut self, pose: Option<RailPose>, y: f32, animate: bool) -> Result<()> {
        let Some(pose) = pose else {
            if self.last.take().is_some() {
                self.clip.SetIsVisible(false)?;
            }
            return Ok(());
        };
        if self.last == Some((pose, y, animate)) {
            return Ok(());
        }
        let rect = pose.rail.unwrap_or(pose.cap);
        self.clip.SetOffset(Vector3 {
            X: 0.0,
            Y: y,
            Z: 0.0,
        })?;
        let last = self.last.map(|(pose, _, _)| pose);
        let finishing = self.last.is_some_and(|(_, _, was)| was && !animate);
        if last != Some(pose) || finishing {
            let previous = last.map(|old| old.rail.unwrap_or(old.cap));
            self.visual.SetSize(Vector2 {
                X: rect.right,
                Y: previous.map_or(rect.bottom, |old| old.bottom.max(rect.bottom)) + rect.width(),
            })?;
            let geometry = &self.shell.geometry;
            let radius = rect.width() * 0.5;
            geometry.SetOffset(Vector2 {
                X: rect.left,
                Y: rect.top,
            })?;
            geometry.SetCornerRadius(Vector2 {
                X: radius,
                Y: radius,
            })?;
            if previous.is_none_or(|old| old.width() != rect.width()) {
                geometry.StopAnimation(h!("Size.Y"))?;
                geometry.SetSize(Vector2 {
                    X: rect.width(),
                    Y: rect.height(),
                })?;
            } else if previous.is_some_and(|old| old.height() != rect.height()) {
                if animate {
                    self.stretch.SetDelayTime(TimeSpan {
                        Duration: if pose.expanded { 360_000 } else { 0 },
                    })?;
                    let target =
                        PropertyValue::CreateSingle(rect.height())?.cast::<IReference<f32>>()?;
                    self.stretch.SetFinalValue(&target)?;
                    geometry.StartAnimation(h!("Size.Y"), &self.stretch)?;
                } else {
                    geometry.StopAnimation(h!("Size.Y"))?;
                    geometry.SetSize(Vector2 {
                        X: rect.width(),
                        Y: rect.height(),
                    })?;
                }
            }
            let (rgb, alpha) = if pose.rail.is_some()
                || (animate
                    && (last.is_some_and(|old| old.rail.is_some())
                        || self.last.is_some_and(|(_, _, was)| was)))
            {
                (
                    0x000000,
                    if pose.hover_cap || pose.hover_scroll {
                        0x48
                    } else {
                        0x34
                    },
                )
            } else {
                (
                    pose.border,
                    if pose.hover_cap {
                        0x66
                    } else if pose.expanded {
                        0x38
                    } else {
                        0
                    },
                )
            };
            self.shell.brush.SetColor(color(rgb, alpha))?;
            if let Some((track, thumb)) = pose.scroll {
                let (a, b) = if pose.hover_scroll {
                    (0x55, 0xdd)
                } else {
                    (0x40, 0xbb)
                };
                self.track
                    .place(track, color(pose.ink, if pose.glass { a } else { 255 }))?;
                self.thumb
                    .place(thumb, color(pose.ink, if pose.glass { b } else { 255 }))?;
            }
        }
        // The tail can remain visible during collapse but never after the rail reaches the cap.
        if !animate && pose.scroll.is_none() {
            for shape in [&self.track, &self.thumb] {
                shape.brush.SetColor(color(0, 0))?;
            }
        }
        self.clip.SetIsVisible(true)?;
        self.last = Some((pose, y, animate));
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct ArrowPose {
    pub(super) rect: Rect,
    pub(super) scale: f32,
    pub(super) expanded: bool,
    pub(super) rgb: u32,
    pub(super) alpha: u8,
}

pub(super) struct Arrow {
    visual: ShapeVisual,
    brush: CompositionColorBrush,
    turn: SpringScalarNaturalMotionAnimation,
    last: Option<ArrowPose>,
}

impl Arrow {
    pub(super) fn new(compositor: &Compositor, children: &VisualCollection) -> Result<Self> {
        let visual = compositor.CreateShapeVisual()?;
        visual.SetSize(Vector2 { X: 20.0, Y: 20.0 })?;
        visual.SetCenterPoint(Vector3 {
            X: 10.0,
            Y: 10.0,
            Z: 0.0,
        })?;
        visual.SetIsVisible(false)?;
        let shapes = visual.Shapes()?;
        let brush = compositor.CreateColorBrush()?;
        let [a, b, c] = panel::chevron_points(Rect::from_xywh(0.0, 0.0, 20.0, 20.0), false, 1.0);
        for (start, end) in [(a, b), (b, c)] {
            let line = compositor.CreateLineGeometry()?;
            line.SetStart(Vector2 {
                X: start.x,
                Y: start.y,
            })?;
            line.SetEnd(Vector2 { X: end.x, Y: end.y })?;
            let shape = compositor.CreateSpriteShapeWithGeometry(&line)?;
            shape.SetStrokeBrush(&brush)?;
            shape.SetStrokeThickness(1.6)?;
            shape.SetStrokeStartCap(CompositionStrokeCap::Round)?;
            shape.SetStrokeEndCap(CompositionStrokeCap::Round)?;
            shapes.Append(&shape)?;
        }
        let turn = compositor.CreateSpringScalarAnimation()?;
        turn.SetDampingRatio(0.78)?;
        turn.SetPeriod(TimeSpan { Duration: 600_000 })?;
        children.InsertAtTop(&visual)?;
        Ok(Self {
            visual,
            brush,
            turn,
            last: None,
        })
    }

    pub(super) fn set(&mut self, pose: Option<ArrowPose>, animate: bool) -> Result<()> {
        if self.last == pose {
            return Ok(());
        }
        let Some(pose) = pose else {
            self.visual.SetIsVisible(false)?;
            self.last = None;
            return Ok(());
        };
        self.visual.SetOffset(Vector3 {
            X: pose.rect.center_x() - 10.0,
            Y: pose.rect.center_y() - 10.0,
            Z: 0.0,
        })?;
        self.visual.SetScale(Vector3 {
            X: pose.scale,
            Y: pose.scale,
            Z: 1.0,
        })?;
        self.brush.SetColor(color(pose.rgb, pose.alpha))?;
        let previous = self.last.map(|old| old.expanded);
        if previous != Some(pose.expanded) {
            let radians = if pose.expanded {
                std::f32::consts::PI
            } else {
                0.0
            };
            let property = h!("RotationAngle");
            if animate && previous.is_some() {
                // Leaving InitialValue unset retargets from the current compositor value.
                let final_value =
                    PropertyValue::CreateSingle(radians)?.cast::<IReference<f32>>()?;
                self.turn.SetFinalValue(&final_value)?;
                self.visual.StartAnimation(property, &self.turn)?;
            } else {
                self.visual.StopAnimation(property)?;
                self.visual.SetRotationAngle(radians)?;
            }
        }
        self.visual.SetIsVisible(true)?;
        self.last = Some(pose);
        Ok(())
    }
}
