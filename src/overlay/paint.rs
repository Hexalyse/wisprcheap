//! A tiny software renderer for the overlay: anti-aliased shapes from signed distance functions (like the
//! tray icons), drawn into a premultiplied BGRA buffer. That's the pixel format of Win32 layered windows and of
//! cairo's ARGB32 surfaces, so both platforms show the buffer as is.
//!
//! Shapes are described in "scene units" (device-independent pixels, centered on the pill); the canvas
//! transform maps them to pixels.

use crate::icons::{len, mic_distance, segment};

/// Straight (non-premultiplied) color, components in 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn hex(rgb: u32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xFF) as f32 / 255.0,
            g: ((rgb >> 8) & 0xFF) as f32 / 255.0,
            b: (rgb & 0xFF) as f32 / 255.0,
            a: 1.0,
        }
    }

    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }
}

/// Axis-aligned box in scene units: (left, top, right, bottom).
pub type Bounds = [f32; 4];

pub fn around(cx: f32, cy: f32, half_w: f32, half_h: f32) -> Bounds {
    [cx - half_w, cy - half_h, cx + half_w, cy + half_h]
}

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// Premultiplied BGRA, top-down rows of `width * 4` bytes.
    pub pixels: Vec<u8>,
    /// Pixel position of the scene origin.
    origin: (f32, f32),
    /// Pixels per scene unit.
    scale: f32,
    /// Multiplies every color's alpha.
    opacity: f32,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height * 4],
            origin: (0.0, 0.0),
            scale: 1.0,
            opacity: 1.0,
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        if (width, height) != (self.width, self.height) {
            *self = Self::new(width, height);
        }
    }

    pub fn clear(&mut self) {
        self.pixels.fill(0);
    }

    /// Where the scene origin lands (pixels), how many pixels a scene unit covers, and a global opacity.
    pub fn set_transform(&mut self, origin: (f32, f32), scale: f32, opacity: f32) {
        self.origin = origin;
        self.scale = scale.max(0.01);
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    /// Blend `color` over every pixel of `bounds`, weighted by `coverage(x, y, pixel_size)` (0..1), where
    /// (x, y) is the pixel center in scene units and `pixel_size` the size of a pixel in scene units.
    fn paint(&mut self, bounds: Bounds, color: Color, coverage: impl Fn(f32, f32, f32) -> f32) {
        let alpha = color.a * self.opacity;
        if alpha <= 0.0 {
            return;
        }
        let (ox, oy, s) = (self.origin.0, self.origin.1, self.scale);
        let clamp_x = |v: f32| (v.max(0.0) as usize).min(self.width);
        let clamp_y = |v: f32| (v.max(0.0) as usize).min(self.height);
        let (x0, x1) = (
            clamp_x((ox + bounds[0] * s).floor()),
            clamp_x((ox + bounds[2] * s).ceil()),
        );
        let (y0, y1) = (
            clamp_y((oy + bounds[1] * s).floor()),
            clamp_y((oy + bounds[3] * s).ceil()),
        );
        let px = 1.0 / s;
        let (r, g, b) = (color.r * 255.0, color.g * 255.0, color.b * 255.0);
        for py in y0..y1 {
            let sy = (py as f32 + 0.5 - oy) * px;
            let row = py * self.width * 4;
            for pxi in x0..x1 {
                let sx = (pxi as f32 + 0.5 - ox) * px;
                let a = alpha * coverage(sx, sy, px).clamp(0.0, 1.0);
                if a <= 0.0 {
                    continue;
                }
                let i = row + pxi * 4;
                let keep = 1.0 - a;
                let d = &mut self.pixels[i..i + 4];
                d[0] = (b * a + d[0] as f32 * keep + 0.5) as u8;
                d[1] = (g * a + d[1] as f32 * keep + 0.5) as u8;
                d[2] = (r * a + d[2] as f32 * keep + 0.5) as u8;
                d[3] = (255.0 * a + d[3] as f32 * keep + 0.5) as u8;
            }
        }
    }

    /// Fill the inside (negative distance) of a shape, with an anti-aliased edge.
    pub fn fill(&mut self, bounds: Bounds, color: Color, sdf: impl Fn(f32, f32) -> f32) {
        self.paint(bounds, color, |x, y, px| coverage(sdf(x, y), px));
    }

    /// Like `fill`, but only inside `clip` (another distance function).
    pub fn fill_clipped(
        &mut self,
        bounds: Bounds,
        color: Color,
        sdf: impl Fn(f32, f32) -> f32,
        clip: impl Fn(f32, f32) -> f32,
    ) {
        self.paint(bounds, color, |x, y, px| {
            coverage(sdf(x, y), px) * coverage(clip(x, y), px)
        });
    }

    /// A soft shadow of a shape: full strength `blur` inside the edge, gone `blur` outside it.
    pub fn shadow(
        &mut self,
        bounds: Bounds,
        color: Color,
        blur: f32,
        sdf: impl Fn(f32, f32) -> f32,
    ) {
        self.paint(bounds, color, |x, y, _| {
            let t = ((sdf(x, y) + blur) / (2.0 * blur)).clamp(0.0, 1.0);
            1.0 - t * t * (3.0 - 2.0 * t)
        });
    }
}

/// Anti-aliasing: how much of a pixel of size `px` a shape at distance `d` covers.
fn coverage(d: f32, px: f32) -> f32 {
    (0.5 - d / px).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Shapes (distances in scene units)
// ---------------------------------------------------------------------------

pub fn circle(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> f32 {
    (x - cx).hypot(y - cy) - r
}

/// Rectangle centered on (cx, cy) with half sizes (hw, hh) and corner radius `r`.
pub fn round_box(x: f32, y: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let r = r.min(hw).min(hh);
    let qx = (x - cx).abs() - hw + r;
    let qy = (y - cy).abs() - hh + r;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// Thick line from (ax, ay) to (bx, by) with round ends.
pub fn capsule(x: f32, y: f32, a: (f32, f32), b: (f32, f32), r: f32) -> f32 {
    segment(
        (x as f64, y as f64),
        (a.0 as f64, a.1 as f64),
        (b.0 as f64, b.1 as f64),
        r as f64,
    ) as f32
}

/// Ring arc with round ends around `center`: radius `radius`, starting at angle `start` (radians, clockwise on
/// screen from 3 o'clock) and spanning `sweep` radians.
pub fn arc(
    x: f32,
    y: f32,
    center: (f32, f32),
    radius: f32,
    start: f32,
    sweep: f32,
    half_width: f32,
) -> f32 {
    use std::f32::consts::TAU;
    let (dx, dy) = (x - center.0, y - center.1);
    let rel = (dy.atan2(dx) - start).rem_euclid(TAU);
    if rel <= sweep {
        return (dx.hypot(dy) - radius).abs() - half_width;
    }
    let end = start + sweep;
    let d0 = (dx - radius * start.cos()).hypot(dy - radius * start.sin());
    let d1 = (dx - radius * end.cos()).hypot(dy - radius * end.sin());
    d0.min(d1) - half_width
}

// ---------------------------------------------------------------------------
// Glyphs, in the 32x32 design space of the tray icons
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Mic,
    MicOff,
    /// Four-pointed sparkles (command mode, like Material's "auto awesome").
    Sparkle,
    Check,
    /// Clipboard (the text was copied, not pasted).
    Clipboard,
    /// Exclamation mark (error).
    Alert,
    /// Padlock (hands-free).
    Lock,
}

fn d32(p: (f64, f64)) -> f64 {
    len(p.0, p.1)
}

fn round_box32(p: (f64, f64), c: (f64, f64), hw: f64, hh: f64, r: f64) -> f64 {
    round_box(
        p.0 as f32, p.1 as f32, c.0 as f32, c.1 as f32, hw as f32, hh as f32, r as f32,
    ) as f64
}

/// Four-pointed star (points up, down, left, right): `r` from the center to a point, `waist` from the center
/// to the inner corners between the points.
fn star4(p: (f64, f64), c: (f64, f64), r: f64, waist: f64) -> f64 {
    // Fold into the first octant: the side from the point (r, 0) to the waist corner at 45 degrees.
    let (mut x, mut y) = ((p.0 - c.0).abs(), (p.1 - c.1).abs());
    if y > x {
        std::mem::swap(&mut x, &mut y);
    }
    let w = waist / std::f64::consts::SQRT_2;
    let (ax, ay, bx, by) = (r, 0.0, w, w);
    let (ex, ey) = (bx - ax, by - ay);
    let h = (((x - ax) * ex + (y - ay) * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
    let d = d32((x - ax - ex * h, y - ay - ey * h));
    // inside when on the center's side of the edge line
    let side = (x - ax) * ey - (y - ay) * ex;
    if side < 0.0 { -d } else { d }
}

fn glyph_distance(glyph: Glyph, p: (f64, f64)) -> f64 {
    match glyph {
        Glyph::Mic => mic_distance(p, false),
        Glyph::MicOff => {
            // the slash, with a gap cut around it in the mic
            let slash = segment(p, (8.0, 7.0), (25.0, 24.0), 1.5);
            mic_distance(p, false).max(-(slash - 1.6)).min(slash)
        }
        Glyph::Sparkle => star4(p, (13.5, 18.5), 11.0, 3.2).min(star4(p, (24.0, 8.0), 5.5, 1.7)),
        Glyph::Check => {
            let a = (8.5, 16.5);
            let b = (13.5, 21.5);
            let c = (23.5, 11.0);
            segment(p, a, b, 2.0).min(segment(p, b, c, 2.0))
        }
        Glyph::Clipboard => {
            let board = round_box32(p, (16.0, 18.0), 8.0, 9.5, 2.2).abs() - 1.4;
            let clip = round_box32(p, (16.0, 8.5), 4.0, 2.2, 1.2);
            let lines = segment(p, (12.0, 17.0), (20.0, 17.0), 1.2).min(segment(
                p,
                (12.0, 21.5),
                (17.5, 21.5),
                1.2,
            ));
            board.min(clip).min(lines)
        }
        Glyph::Alert => {
            segment(p, (16.0, 7.0), (16.0, 18.5), 2.3).min(d32((p.0 - 16.0, p.1 - 24.5)) - 2.4)
        }
        Glyph::Lock => {
            let body = round_box32(p, (16.0, 20.5), 8.0, 6.5, 2.0);
            let (cx, cy, radius, half) = (16.0, 12.5, 5.0, 1.5);
            let shackle = if p.1 <= cy {
                (d32((p.0 - cx, p.1 - cy)) - radius).abs() - half
            } else {
                segment(p, (cx - radius, cy), (cx - radius, 15.0), half).min(segment(
                    p,
                    (cx + radius, cy),
                    (cx + radius, 15.0),
                    half,
                ))
            };
            body.min(shackle)
        }
    }
}

/// Distance to a glyph drawn in a `size` x `size` box centered on (cx, cy).
pub fn glyph(glyph: Glyph, x: f32, y: f32, cx: f32, cy: f32, size: f32) -> f32 {
    let k = 32.0 / size as f64;
    let p = ((x - cx) as f64 * k + 16.0, (y - cy) as f64 * k + 16.0);
    (glyph_distance(glyph, p) / k) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_have_the_expected_insides() {
        assert!(circle(0.0, 0.0, 0.0, 0.0, 5.0) < 0.0);
        assert!((circle(10.0, 0.0, 0.0, 0.0, 5.0) - 5.0).abs() < 1e-5);
        assert!(round_box(0.0, 0.0, 0.0, 0.0, 10.0, 4.0, 4.0) < 0.0);
        assert!((round_box(12.0, 0.0, 0.0, 0.0, 10.0, 4.0, 4.0) - 2.0).abs() < 1e-5);
        // a quarter arc from 3 o'clock to 6 o'clock (screen y goes down)
        let quarter = std::f32::consts::FRAC_PI_2;
        assert!(arc(0.0, 10.0, (0.0, 0.0), 10.0, 0.0, quarter, 1.0) < 0.0);
        assert!(arc(0.0, -10.0, (0.0, 0.0), 10.0, 0.0, quarter, 1.0) > 5.0);
        for g in [
            Glyph::Mic,
            Glyph::Sparkle,
            Glyph::Check,
            Glyph::Alert,
            Glyph::Lock,
        ] {
            // something is drawn, and nothing near the corners of the box
            let inside = (0..32)
                .any(|y| (0..32).any(|x| glyph(g, x as f32, y as f32, 16.0, 16.0, 32.0) < 0.0));
            assert!(inside, "{g:?}");
            assert!(glyph(g, 0.5, 0.5, 16.0, 16.0, 32.0) > 0.0, "{g:?}");
        }
    }

    #[test]
    fn blends_premultiplied() {
        let mut c = Canvas::new(4, 4);
        c.set_transform((0.0, 0.0), 1.0, 1.0);
        c.fill(
            [0.0, 0.0, 4.0, 4.0],
            Color::hex(0xFF0000).with_alpha(0.5),
            |_, _| -10.0,
        );
        assert_eq!(&c.pixels[0..4], &[0, 0, 128, 128]);
        c.fill([0.0, 0.0, 4.0, 4.0], Color::hex(0x0000FF), |_, _| -10.0);
        assert_eq!(&c.pixels[0..4], &[255, 0, 0, 255]);
    }
}
