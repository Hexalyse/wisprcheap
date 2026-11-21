//! The edge glow drawn in software, for Windows' layered windows: the same band of light as the GTK4 version
//! (`glow::LAYERS`, `glow::shape`, `glow::wave`), with each layer's blur approximated by a soft edge along the
//! distance to its outline. Pixels are premultiplied BGRA, the format of `UpdateLayeredWindow`.
//!
//! The glow only lights a U along the bottom and sides of the window, so it's shown in three pieces (`Piece`),
//! and the transparent middle is never drawn or copied.

use super::glow::{GlowFrame, LAYERS, shape, wave};
use super::paint::Color;

/// How far above the bottom edge any layer can light, at full swell (device-independent pixels): the corner
/// of the widest layer (2.6 times its 32 px band), plus its blur.
const BOTTOM_ZONE: f32 = 100.0;
/// How far in from the side edges any layer can light: the widest band, plus its blur.
const SIDE_ZONE: f32 = 48.0;
/// Above the top of the sides' climb, the light fades out within the widest blur.
const TOP_MARGIN: f32 = 16.0;

/// The parts of the window the glow lights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece {
    /// Full width, along the bottom edge.
    Bottom,
    /// Up the left edge, above the bottom piece.
    Left,
    /// Up the right edge, above the bottom piece.
    Right,
}

impl Piece {
    pub const ALL: [Self; 3] = [Self::Bottom, Self::Left, Self::Right];
}

/// The window the glow lights, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub width: i32,
    pub height: i32,
    /// Physical pixels per device-independent pixel.
    pub scale: f32,
    /// Radius of the window's own rounded corners (device-independent pixels), so the glow doesn't spill
    /// past them.
    pub rounding: f32,
}

impl Target {
    /// Where `piece` goes, relative to the window's top-left corner: (x, y, width, height), empty when the
    /// window is too small for it.
    pub fn rect(&self, piece: Piece) -> (i32, i32, i32, i32) {
        let (w, h) = (self.width.max(0), self.height.max(0));
        let px = |v: f32| (v * self.scale).ceil() as i32;
        let lit = px(shape(1.0, h as f32 / self.scale).2 + TOP_MARGIN).min(h);
        let bottom = px(BOTTOM_ZONE).min(lit);
        let side = px(SIDE_ZONE).min(w / 2);
        let top = h - lit;
        match piece {
            Piece::Bottom => (0, h - bottom, w, bottom),
            Piece::Left => (0, top, side, lit - bottom),
            Piece::Right => (w - side, top, side, lit - bottom),
        }
    }
}

fn smoothstep(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

/// One layer of the band, over a window `w` x `h` (device-independent pixels).
struct Layer {
    color: Color,
    alpha: f32,
    blur: f32,
    band: f32,
    radius: f32,
    corner: f32,
    /// How far the sides climb above the corners.
    climb: f32,
    /// Height of the inner edge above the bottom, for each column drawn, and the highest.
    edge: Vec<f32>,
    edge_max: f32,
}

impl Layer {
    fn new(
        frame: &GlowFrame,
        (scale, color, alpha, blur): (f32, usize, f32, f32),
        w: f32,
        h: f32,
        columns: impl Iterator<Item = f32>,
    ) -> Self {
        // the same outline as the GTK4 version (`glow_outline`)
        let (base, amplitude, reach) = shape(frame.swell, h);
        let band = base * scale;
        let radius = band * 1.6;
        let corner = band + radius;
        let reach = reach * (0.55 + 0.45 * scale.min(1.0));
        // the wave flattens out near the corners, so the band meets the sides at its base thickness
        let edge: Vec<f32> = columns
            .map(|x| {
                let ramp = smoothstep((x.min(w - x) - corner) / (3.0 * corner));
                band + scale * wave(x, frame.time, amplitude) * ramp
            })
            .collect();
        let edge_max = edge.iter().copied().fold(band, f32::max);
        Self {
            color: frame.colors[color],
            alpha,
            blur,
            band,
            radius,
            corner,
            climb: reach - corner,
            edge,
            edge_max,
        }
    }

    /// Signed distance from the layer's inner outline at `x` across and `v` above the bottom edge: positive
    /// in the light. `column` indexes `edge`.
    fn distance(&self, x: f32, v: f32, w: f32, column: usize) -> f32 {
        if v < self.corner {
            let corner = self.corner.min(w / 2.0);
            // the inner corners: quarter circles from the sides into the bottom band
            if x < corner {
                return (x - self.corner).hypot(v - self.corner) - self.radius;
            }
            if x > w - corner {
                return (x - (w - self.corner)).hypot(v - self.corner) - self.radius;
            }
            return self.edge[column] - v;
        }
        // the sides: full thickness, easing to nothing at the top of the climb, and out of the light above
        let u = v - self.corner;
        let side =
            self.band * (1.0 - smoothstep(u / self.climb.max(1.0))) - (u - self.climb).max(0.0);
        (side - x).max(x - (w - side))
    }

    /// The columns this layer may light at height `v`: those before the first index, and from the second
    /// one. `xs` are the columns' positions, in order.
    fn lit_columns(&self, v: f32, w: f32, xs: &[f32]) -> (usize, usize) {
        let (left, right) = if v >= self.corner {
            let u = v - self.corner;
            let side =
                self.band * (1.0 - smoothstep(u / self.climb.max(1.0))) - (u - self.climb).max(0.0);
            (side + self.blur, w - side - self.blur)
        } else if v >= self.edge_max + self.blur {
            // above the bottom band: only the corners
            let corner = self.corner.min(w / 2.0);
            (corner, w - corner)
        } else {
            return (xs.len(), xs.len());
        };
        (
            xs.partition_point(|&x| x < left),
            xs.partition_point(|&x| x <= right),
        )
    }

    fn coverage(&self, distance: f32) -> f32 {
        // a blurred edge goes from dark to fully lit over about twice its blur radius
        self.alpha * smoothstep((distance + self.blur) / (2.0 * self.blur))
    }
}

/// How much of the pixel at `x`, `v` the window's own rounded bottom corners cover.
fn inside_rounding(x: f32, v: f32, w: f32, rounding: f32, scale: f32) -> f32 {
    if rounding <= 0.0 || v >= rounding {
        return 1.0;
    }
    let dx = if x < rounding {
        rounding - x
    } else if x > w - rounding {
        x - (w - rounding)
    } else {
        return 1.0;
    };
    let outside = dx.hypot(rounding - v) - rounding;
    (0.5 - outside * scale).clamp(0.0, 1.0)
}

/// Draw `piece` of the glow over `target` into `pixels` (premultiplied BGRA, rows of `Target::rect`'s width).
pub fn draw(pixels: &mut [u8], target: &Target, piece: Piece, frame: &GlowFrame) {
    let (x0, y0, width, height) = target.rect(piece);
    let (width, height) = (width.max(0) as usize, height.max(0) as usize);
    let pixels = &mut pixels[..width * height * 4];
    if frame.opacity <= 0.0 || width == 0 || height == 0 {
        pixels.fill(0);
        return;
    }
    let scale = target.scale;
    let (w, h) = (target.width as f32 / scale, target.height as f32 / scale);
    let xs: Vec<f32> = (0..width)
        .map(|column| (x0 as f32 + column as f32 + 0.5) / scale)
        .collect();
    let layers = LAYERS.map(|layer| Layer::new(frame, layer, w, h, xs.iter().copied()));
    // Between the corners, a lit layer is in its bottom band: its distance is just to the wave.
    let corners = layers
        .iter()
        .map(|l| l.corner.min(w / 2.0))
        .fold(target.rounding, f32::max);
    let middle = (
        xs.partition_point(|&x| x < corners),
        xs.partition_point(|&x| x <= w - corners),
    );
    for (row, line) in pixels.chunks_exact_mut(width * 4).enumerate() {
        let v = (target.height as f32 - (y0 + row as i32) as f32 - 0.5) / scale;
        // Most of the bottom piece is dark for most layers: only work out the columns each one may light.
        let lit = layers.each_ref().map(|layer| layer.lit_columns(v, w, &xs));
        let dark_from = lit.iter().map(|l| l.0).max().unwrap_or(0);
        let dark_to = lit.iter().map(|l| l.1).min().unwrap_or(0).max(dark_from);
        line[dark_from * 4..dark_to * 4].fill(0);
        let columns = (0..dark_from).chain(dark_to..width);
        for column in columns {
            let pixel = &mut line[column * 4..column * 4 + 4];
            let x = xs[column];
            let between = column >= middle.0 && column < middle.1;
            // premultiplied, each layer over the ones before
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for (layer, &(before, from)) in layers.iter().zip(&lit) {
                if column >= before && column < from {
                    continue;
                }
                let distance = if between {
                    layer.edge[column] - v
                } else {
                    layer.distance(x, v, w, column)
                };
                let alpha = layer.coverage(distance);
                if alpha > 0.0 {
                    let rest = 1.0 - alpha;
                    r = layer.color.r * alpha + r * rest;
                    g = layer.color.g * alpha + g * rest;
                    b = layer.color.b * alpha + b * rest;
                    a = alpha + a * rest;
                }
            }
            let k = if between {
                255.0 * frame.opacity
            } else {
                255.0 * frame.opacity * inside_rounding(x, v, w, target.rounding, scale)
            };
            pixel.copy_from_slice(&[
                (b * k + 0.5) as u8,
                (g * k + 0.5) as u8,
                (r * k + 0.5) as u8,
                (a * k + 0.5) as u8,
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::glow::GlowLook;
    use super::*;

    fn frame(swell: f32) -> GlowFrame {
        GlowFrame {
            opacity: 1.0,
            colors: GlowLook::Dictation.palette(),
            time: 1.3,
            swell,
        }
    }

    const TARGET: Target = Target {
        width: 1200,
        height: 800,
        scale: 1.0,
        rounding: 0.0,
    };

    fn render(target: &Target, piece: Piece, frame: &GlowFrame) -> (Vec<u8>, usize) {
        let (_, _, w, h) = target.rect(piece);
        let mut pixels = vec![0xAB; (w * h * 4) as usize];
        draw(&mut pixels, target, piece, frame);
        (pixels, w as usize)
    }

    fn alpha(pixels: &[u8], width: usize, x: usize, y: usize) -> u8 {
        pixels[(y * width + x) * 4 + 3]
    }

    #[test]
    fn pieces_frame_the_bottom_and_sides() {
        let (x, y, w, h) = TARGET.rect(Piece::Bottom);
        assert_eq!((x, y + h, w), (0, 800, 1200));
        let (lx, ly, lw, lh) = TARGET.rect(Piece::Left);
        let (rx, ry, rw, rh) = TARGET.rect(Piece::Right);
        assert_eq!((lx, ly + lh), (0, y));
        assert_eq!((rx + rw, ry, rw, rh), (1200, ly, lw, lh));
        // as high as the sides climb at full swell, plus the blur
        assert_eq!(ly, 800 - (0.38f32 * 800.0 + TOP_MARGIN).ceil() as i32);
        // a tiny window: everything the glow climbs fits in the bottom piece
        let tiny = Target {
            width: 60,
            height: 50,
            ..TARGET
        };
        let lit = (0.38f32 * 50.0 + TOP_MARGIN).ceil() as i32;
        assert_eq!(tiny.rect(Piece::Bottom), (0, 50 - lit, 60, lit));
        assert_eq!(tiny.rect(Piece::Left).3, 0);
    }

    #[test]
    fn brightest_at_the_edges_and_dark_inside() {
        let frame = frame(0.5);
        let (bottom, width) = render(&TARGET, Piece::Bottom, &frame);
        let rows = bottom.len() / 4 / width;
        let at_edge = alpha(&bottom, width, 600, rows - 1);
        assert!(at_edge > 150, "{at_edge}");
        assert_eq!(alpha(&bottom, width, 600, 0), 0);
        // the corner is lit
        assert!(alpha(&bottom, width, 0, rows - 1) > 150);
        // blue: premultiplied channels never exceed alpha
        for pixel in bottom.as_chunks::<4>().0 {
            assert!(pixel[0] <= pixel[3] && pixel[2] <= pixel[3], "{pixel:?}");
        }
        let (left, width) = render(&TARGET, Piece::Left, &frame);
        let rows = left.len() / 4 / width;
        assert!(alpha(&left, width, 0, rows - 1) > 150);
        assert_eq!(alpha(&left, width, width - 1, rows - 1), 0);
        // the climb fades out before the top of the piece
        assert_eq!(alpha(&left, width, 0, 0), 0);
        let (right, width) = render(&TARGET, Piece::Right, &frame);
        assert!(alpha(&right, width, width - 1, rows - 1) > 150);
    }

    #[test]
    fn the_pieces_hold_all_the_light() {
        // just outside each piece, at full swell, nothing is lit
        let frame = frame(1.0);
        let (w, h) = (TARGET.width as f32, TARGET.height as f32);
        let layers =
            LAYERS.map(|layer| Layer::new(&frame, layer, w, h, (0..1200).map(|x| x as f32 + 0.5)));
        let (_, by, _, _) = TARGET.rect(Piece::Bottom);
        let (_, ly, lw, _) = TARGET.rect(Piece::Left);
        for layer in &layers {
            for column in (lw as usize..1200 - lw as usize).step_by(7) {
                let v = h - by as f32 + 0.5;
                let lit = layer.coverage(layer.distance(column as f32 + 0.5, v, w, column));
                assert_eq!(lit, 0.0, "above the bottom piece at {column}");
            }
            for v in (800 - by as usize..800 - ly as usize).step_by(5) {
                let lit =
                    layer.coverage(layer.distance(lw as f32 + 0.5, v as f32 + 0.5, w, lw as usize));
                assert_eq!(lit, 0.0, "beside the left piece at {v}");
            }
            let lit = layer.coverage(layer.distance(0.5, h - ly as f32 + 0.5, w, 0));
            assert_eq!(lit, 0.0, "above the left piece");
        }
    }

    #[test]
    fn follows_the_window_rounded_corners() {
        let rounded = Target {
            rounding: 8.0,
            ..TARGET
        };
        let frame = frame(0.5);
        let (pixels, width) = render(&rounded, Piece::Bottom, &frame);
        let rows = pixels.len() / 4 / width;
        assert_eq!(alpha(&pixels, width, 0, rows - 1), 0);
        assert_eq!(alpha(&pixels, width, width - 1, rows - 1), 0);
        assert!(alpha(&pixels, width, 8, rows - 1) > 150);
    }

    /// Writes the glow over a 1200 x 800 window, quiet then speaking, to target/overlay-preview, to look at it
    /// without recording: `cargo test glow_previews -- --ignored`.
    #[test]
    #[ignore]
    fn glow_previews() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/overlay-preview");
        std::fs::create_dir_all(&dir).unwrap();
        let target = Target {
            rounding: 8.0,
            ..TARGET
        };
        let (w, h) = (target.width as usize, target.height as usize);
        for (name, swell) in [("glow-quiet", 0.0), ("glow-speaking", 0.9)] {
            let mut frame = frame(swell);
            frame.opacity = if swell > 0.0 { 1.0 } else { 0.75 };
            let mut window = vec![0u8; w * h * 4];
            for piece in Piece::ALL {
                let (x0, y0, pw, _) = target.rect(piece);
                let (pixels, _) = render(&target, piece, &frame);
                for (row, line) in pixels.chunks_exact(pw as usize * 4).enumerate() {
                    let at = ((y0 as usize + row) * w + x0 as usize) * 4;
                    window[at..at + line.len()].copy_from_slice(line);
                }
            }
            let bg = Color::hex(0x1E1E24);
            let rgba: Vec<u8> = window
                .chunks(4)
                .flat_map(|px| {
                    let a = px[3] as f32 / 255.0;
                    let mix = |c: u8, b: f32| (c as f32 + b * 255.0 * (1.0 - a)).round() as u8;
                    [mix(px[2], bg.r), mix(px[1], bg.g), mix(px[0], bg.b), 255]
                })
                .collect();
            let file = std::fs::File::create(dir.join(format!("{name}.png"))).unwrap();
            let mut encoder = png::Encoder::new(file, w as u32, h as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgba)
                .unwrap();
        }
    }

    #[test]
    fn hidden_is_transparent_and_scales_with_dpi() {
        let mut hidden = frame(0.5);
        hidden.opacity = 0.0;
        let (pixels, _) = render(&TARGET, Piece::Bottom, &hidden);
        assert!(pixels.iter().all(|&b| b == 0));
        let hidpi = Target {
            width: 1800,
            height: 1200,
            scale: 1.5,
            rounding: 0.0,
        };
        assert_eq!(hidpi.rect(Piece::Bottom).3, 150);
        let (pixels, width) = render(&hidpi, Piece::Bottom, &frame(0.5));
        assert!(alpha(&pixels, width, 900, 149) > 150);
    }
}
