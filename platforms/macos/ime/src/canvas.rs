//! AppKit 画布:把平台无关的绘制指令画到当前 NSGraphicsContext。
//!
//! 面板坐标是逻辑点,和 macOS 的视图坐标一致;Retina 缩放由 AppKit
//! 的矢量绘制自动处理。文字与图形都走系统字体与贝塞尔路径。

use lueying_render::paint::{Canvas, Color, Point};
use lueying_render::panel::Rect;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBezierPath, NSColor, NSCompositingOperation, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSGraphicsContext, NSImage, NSShadow, NSStringDrawing,
};
use objc2_foundation::{NSAttributedStringKey, NSDictionary, NSPoint, NSRect, NSSize, NSString};

/// 画布。`sprites` 是动画包的帧图,按帧号索引;候选窗没有角色,传空即可。
pub struct AppKitCanvas<'a> {
    pub sprites: &'a [Retained<NSImage>],
}

/// 独立的测宽器:布局在画之前就要知道文字宽度。
pub struct AppKitMeasure;

fn ns_color(color: Color) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        f64::from((color.rgb >> 16) & 0xff) / 255.0,
        f64::from((color.rgb >> 8) & 0xff) / 255.0,
        f64::from(color.rgb & 0xff) / 255.0,
        f64::from(color.alpha) / 255.0,
    )
}

fn ns_rect(rect: Rect) -> NSRect {
    NSRect::new(
        NSPoint::new(f64::from(rect.left), f64::from(rect.top)),
        NSSize::new(f64::from(rect.width()), f64::from(rect.height())),
    )
}

fn attributes(size: f32, color: Color) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let font = NSFont::systemFontOfSize(f64::from(size));
    let color = ns_color(color);
    let font: Retained<AnyObject> = unsafe { Retained::cast_unchecked(font) };
    let color: Retained<AnyObject> = unsafe { Retained::cast_unchecked(color) };
    // SAFETY:两个属性名都是 AppKit 导出的常量字符串。
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    NSDictionary::from_retained_objects(&keys, &[font, color])
}

impl Canvas for AppKitCanvas<'_> {
    fn fill_rect(&mut self, rect: Rect, color: Color) {
        ns_color(color).setFill();
        NSBezierPath::fillRect(ns_rect(rect));
    }

    fn stroke_rect(&mut self, rect: Rect, width: f32, color: Color) {
        ns_color(color).setStroke();
        let path = NSBezierPath::bezierPathWithRect(ns_rect(rect));
        path.setLineWidth(f64::from(width));
        path.stroke();
    }

    fn fill_rrect(&mut self, rect: Rect, radius: f32, color: Color) {
        ns_color(color).setFill();
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            ns_rect(rect),
            f64::from(radius),
            f64::from(radius),
        );
        path.fill();
    }

    fn stroke_rrect(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        ns_color(color).setStroke();
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            ns_rect(rect),
            f64::from(radius),
            f64::from(radius),
        );
        path.setLineWidth(f64::from(width));
        path.stroke();
    }

    fn shadow_rrect(&mut self, rect: Rect, radius: f32, blur: f32, color: Color) {
        if color.alpha == 0 {
            return;
        }
        let shadow = NSShadow::new();
        shadow.setShadowOffset(NSSize::new(0.0, -2.0));
        shadow.setShadowBlurRadius(f64::from(blur));
        shadow.setShadowColor(Some(&ns_color(color)));
        NSGraphicsContext::saveGraphicsState_class();
        shadow.set();
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            ns_rect(rect),
            f64::from(radius),
            f64::from(radius),
        );
        ns_color(color).setFill();
        path.fill();
        NSGraphicsContext::restoreGraphicsState_class();
    }

    fn text(
        &mut self,
        content: &str,
        origin: Point,
        size: f32,
        color: Color,
        max_width: Option<f32>,
    ) {
        let text = NSString::from_str(content);
        let attrs = attributes(size, color);
        let measured = unsafe { text.sizeWithAttributes(Some(&attrs)) };
        let point = NSPoint::new(
            f64::from(origin.x),
            f64::from(origin.y) - measured.height * 0.5,
        );
        if let Some(max_width) = max_width {
            NSGraphicsContext::saveGraphicsState_class();
            let clip = NSBezierPath::bezierPathWithRect(NSRect::new(
                point,
                NSSize::new(f64::from(max_width), measured.height.max(1.0)),
            ));
            clip.setClip();
            unsafe { text.drawAtPoint_withAttributes(point, Some(&attrs)) };
            NSGraphicsContext::restoreGraphicsState_class();
        } else {
            unsafe { text.drawAtPoint_withAttributes(point, Some(&attrs)) };
        }
    }

    fn measure(&mut self, content: &str, size: f32) -> f32 {
        measure_text(content, size)
    }

    fn draw_sprite(&mut self, sprite: usize, rect: Rect) {
        let Some(image) = self.sprites.get(sprite) else {
            return;
        };
        image.drawInRect_fromRect_operation_fraction(
            ns_rect(rect),
            NSRect::ZERO,
            NSCompositingOperation::SourceOver,
            1.0,
        );
    }

    fn text_centered(&mut self, content: &str, rect: Rect, size: f32, color: Color) {
        let text = NSString::from_str(content);
        let attrs = attributes(size, color);
        let measured = unsafe { text.sizeWithAttributes(Some(&attrs)) };
        let point = NSPoint::new(
            f64::from(rect.center_x()) - measured.width * 0.5,
            f64::from(rect.center_y()) - measured.height * 0.5,
        );
        unsafe { text.drawAtPoint_withAttributes(point, Some(&attrs)) };
    }
}

impl lueying_render::panel::Measure for AppKitMeasure {
    fn width(&self, text: &str, size: f32) -> f32 {
        measure_text(text, size)
    }
}

fn measure_text(content: &str, size: f32) -> f32 {
    let text = NSString::from_str(content);
    let attrs = attributes(size, Color::opaque(0x000000));
    unsafe { text.sizeWithAttributes(Some(&attrs)) }.width as f32
}
