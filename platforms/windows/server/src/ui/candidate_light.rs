//! Ambient light from sparse desktop samples; never allocates a captured frame.

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};

use super::place;

pub(super) type PanelRect = (i32, i32, i32, i32);
const INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Light {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) strength: f32,
    rgb: [f32; 3],
}

impl Default for Light {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            strength: 0.65,
            rgb: [128.0; 3],
        }
    }
}

impl Light {
    pub(super) fn tint(self, base: u32, amount: f32) -> u32 {
        let peak = self.rgb.into_iter().fold(0.0_f32, f32::max);
        // Leave neutral and near-black surroundings alone; do not clip color into white.
        let amount = amount * (peak / 85.0).clamp(0.0, 1.0);
        self.rgb
            .into_iter()
            .zip([16, 8, 0])
            .fold(0, |color, (sample, shift)| {
                let channel = ((base >> shift) & 255) as f32;
                let value = channel * (1.0 - amount * (1.0 - sample / peak.max(1.0)));
                color | ((value.round() as u32) << shift)
            })
    }

    fn from_samples(samples: &[(f32, f32, [f32; 3])]) -> Option<Self> {
        if samples.len() < 3 {
            return None;
        }
        let (mut x, mut y, mut energy) = (0.0, 0.0, 0.0);
        let (mut sample_x, mut sample_y) = (0.0, 0.0);
        let brightness = |color: [f32; 3]| {
            let luma = 0.213 * color[0] + 0.715 * color[1] + 0.072 * color[2];
            (0.7 * luma + 0.3 * color.into_iter().fold(0.0_f32, f32::max)) / 255.0
        };
        for &(sx, sy, color) in samples {
            let weight = 0.10 + brightness(color).powi(2);
            x += sx * weight;
            y += sy * weight;
            energy += weight;
            sample_x += sx;
            sample_y += sy;
        }
        // Missing points at monitor edges should not turn uniform light into a directional key.
        let count = samples.len() as f32;
        let (x, y) = (x / energy - sample_x / count, y / energy - sample_y / count);
        let mut rgb = [0.0; 3];
        let mut key_energy = 0.0;
        for &(sx, sy, color) in samples {
            // The specular lobe reflects the bright sector, not the mean color of the desktop.
            let direction = (1.0 + 2.5 * (x * sx + y * sy)).max(0.15);
            let weight = (0.01 + brightness(color).powi(4)) * direction;
            key_energy += weight;
            for (value, channel) in rgb.iter_mut().zip(color) {
                *value += weight * channel;
            }
        }
        Some(Self {
            x,
            y,
            strength: (energy / count - 0.10).max(0.0).sqrt(),
            rgb: rgb.map(|v| v / key_energy),
        })
    }

    fn approach(self, next: Self) -> Self {
        let toward = |old: f32, new: f32, deadband: f32| {
            if (old - new).abs() <= deadband {
                old
            } else {
                old + (new - old) * 0.40
            }
        };
        Self {
            x: toward(self.x, next.x, 0.025),
            y: toward(self.y, next.y, 0.025),
            strength: toward(self.strength, next.strength, 0.025),
            rgb: std::array::from_fn(|i| toward(self.rgb[i], next.rgb[i], 5.0)),
        }
    }
}

#[derive(Default)]
pub(super) struct Ambient {
    light: Light,
    next: Option<Instant>,
}

impl Ambient {
    pub(super) fn light(&self) -> Light {
        self.light
    }

    pub(super) fn refresh(
        &mut self,
        rect: PanelRect,
        occlusion: Option<PanelRect>,
        scale: f32,
    ) -> bool {
        let now = Instant::now();
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        let first = self.next.is_none();
        self.next = Some(now + INTERVAL);
        let Some(sample) = sample(rect, occlusion, scale) else {
            return false;
        };
        let light = if first {
            sample
        } else {
            self.light.approach(sample)
        };
        let changed = self.light != light;
        self.light = light;
        changed
    }

    pub(super) fn timeout(&self) -> Option<Duration> {
        self.next
            .map(|next| next.saturating_duration_since(Instant::now()))
    }
}

fn sample((x, y, w, h): PanelRect, occlusion: Option<PanelRect>, scale: f32) -> Option<Light> {
    let gap = (20.0 * scale).round().max(8.0) as i32;
    let area = place::work_area_at(x + w / 2, y + h / 2);
    let points = [
        (x + w / 5, y - gap, -0.6, -1.0),
        (x + w / 2, y - gap, 0.0, -1.0),
        (x + w * 4 / 5, y - gap, 0.6, -1.0),
        (x + w + gap, y + h / 2, 1.0, 0.0),
        (x + w * 4 / 5, y + h + gap, 0.6, 1.0),
        (x + w / 2, y + h + gap, 0.0, 1.0),
        (x + w / 5, y + h + gap, -0.6, 1.0),
        (x - gap, y + h / 2, -1.0, 0.0),
    ];
    let mut samples = [(0.0, 0.0, [0.0; 3]); 8];
    let mut count = 0;
    unsafe {
        let dc = GetDC(None);
        if dc.is_invalid() {
            return None;
        }
        for (px, py, dx, dy) in points {
            if px < area.left
                || px >= area.right
                || py < area.top
                || py >= area.bottom
                || occlusion
                    .is_some_and(|(l, t, ow, oh)| px >= l && px < l + ow && py >= t && py < t + oh)
            {
                continue;
            }
            let rgb = GetPixel(dc, px, py).0;
            if rgb == u32::MAX {
                continue;
            }
            samples[count] = (
                dx,
                dy,
                [0, 8, 16].map(|shift| ((rgb >> shift) & 255) as f32),
            );
            count += 1;
        }
        ReleaseDC(None, dc);
    }
    Light::from_samples(&samples[..count])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bright_side_pulls_the_reflection_and_neutral_light_preserves_tint() {
        let light = Light::from_samples(&[
            (-1.0, 0.0, [25.0; 3]),
            (1.0, 0.0, [240.0; 3]),
            (0.0, -1.0, [80.0; 3]),
        ])
        .unwrap();
        assert!(light.x > 0.5);
        assert_eq!(light.tint(0xacc5d0, 0.2), 0xacc5d0);
        assert_eq!(light.approach(light), light);
        assert!(Light::from_samples(&[]).is_none());
    }

    #[test]
    fn white_glint_retains_the_color_of_the_bright_side() {
        let reflected = |bright, dim| {
            Light::from_samples(&[(1.0, 0.0, bright), (-1.0, 0.0, dim), (0.0, 1.0, [38.0; 3])])
                .unwrap()
                .tint(0xffffff, 0.64)
        };
        let warm = reflected([255.0, 173.0, 72.0], [29.0, 55.0, 112.0]);
        let cool = reflected([75.0, 168.0, 255.0], [112.0, 55.0, 29.0]);
        assert!((warm >> 16) > (warm & 255) + 80, "{warm:06x}");
        assert!((cool & 255) > (cool >> 16) + 80, "{cool:06x}");
    }

    #[test]
    fn uniform_light_has_no_direction_even_with_missing_edges() {
        let light = Light::from_samples(&[
            (-1.0, 0.0, [220.0; 3]),
            (-0.6, -1.0, [220.0; 3]),
            (0.0, -1.0, [220.0; 3]),
        ])
        .unwrap();
        assert!(light.x.abs() < 0.001 && light.y.abs() < 0.001, "{light:?}");
    }
}
