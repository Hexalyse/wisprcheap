//! Tray icons drawn in code (a white microphone on a colored disc).
//! Shapes are signed distance fields in a 32x32 design space, which gives anti-aliased edges at every size.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconName {
    Idle,
    Recording,
    Processing,
    Paused,
}

impl IconName {
    pub const ALL: [IconName; 4] = [
        IconName::Idle,
        IconName::Recording,
        IconName::Processing,
        IconName::Paused,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            IconName::Idle => "idle",
            IconName::Recording => "recording",
            IconName::Processing => "processing",
            IconName::Paused => "paused",
        }
    }

    fn color(self) -> [f64; 3] {
        match self {
            IconName::Idle => [75.0, 85.0, 99.0],         // grey
            IconName::Recording => [220.0, 38.0, 38.0],   // red
            IconName::Processing => [245.0, 158.0, 11.0], // amber
            IconName::Paused => [156.0, 163.0, 175.0],    // light grey + slash
        }
    }
}

const SIZES: [u32; 7] = [16, 20, 24, 32, 40, 48, 64];

fn len(x: f64, y: f64) -> f64 {
    x.hypot(y)
}

fn segment(p: (f64, f64), a: (f64, f64), b: (f64, f64), r: f64) -> f64 {
    let (px, py) = (p.0 - a.0, p.1 - a.1);
    let (bx, by) = (b.0 - a.0, b.1 - a.1);
    let denom = bx * bx + by * by;
    let h = ((px * bx + py * by) / if denom == 0.0 { 1.0 } else { denom }).clamp(0.0, 1.0);
    len(px - bx * h, py - by * h) - r
}

/// Lower half of a ring (the mic holder), with round ends.
fn holder(p: (f64, f64)) -> f64 {
    let (cx, cy, radius, half) = (16.0, 14.0, 7.5, 1.25);
    if p.1 >= cy {
        return (len(p.0 - cx, p.1 - cy) - radius).abs() - half;
    }
    len(p.0 - (cx - radius), p.1 - cy).min(len(p.0 - (cx + radius), p.1 - cy)) - half
}

fn mic_distance(p: (f64, f64), slash: bool) -> f64 {
    let mut d = segment(p, (16.0, 9.5), (16.0, 14.5), 4.0) // capsule
        .min(holder(p))
        .min(segment(p, (16.0, 21.5), (16.0, 25.5), 1.25)) // stem
        .min(segment(p, (11.5, 25.8), (20.5, 25.8), 1.25)); // base
    if slash {
        d = d.min(segment(p, (8.0, 8.0), (24.0, 24.0), 1.6));
    }
    d
}

/// Top-down RGBA pixels.
pub fn render_rgba(name: IconName, size: u32) -> Vec<u8> {
    let [r, g, b] = name.color();
    let px = 32.0 / size as f64; // design units per pixel
    let coverage = |d: f64| (0.5 - d / px).clamp(0.0, 1.0);
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let p = ((x as f64 + 0.5) * px, (y as f64 + 0.5) * px);
            let disc = coverage(len(p.0 - 16.0, p.1 - 16.0) - 15.5);
            let mic = coverage(mic_distance(p, name == IconName::Paused));
            let i = ((y * size + x) * 4) as usize;
            out[i] = (r + (255.0 - r) * mic).round() as u8;
            out[i + 1] = (g + (255.0 - g) * mic).round() as u8;
            out[i + 2] = (b + (255.0 - b) * mic).round() as u8;
            out[i + 3] = (disc * 255.0).round() as u8;
        }
    }
    out
}

/// Multi-size .ico (32-bit BMP entries, bottom-up BGRA, empty AND mask).
pub fn encode_ico(name: IconName) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| {
            let rgba = render_rgba(name, size);
            let mut pixels = Vec::with_capacity(rgba.len());
            for y in (0..size).rev() {
                for x in 0..size {
                    let i = ((y * size + x) * 4) as usize;
                    pixels.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
                }
            }
            let and_mask = vec![0u8; (size.div_ceil(32) * 4 * size) as usize];
            let mut data = Vec::with_capacity(40 + pixels.len() + and_mask.len());
            data.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER size
            data.extend_from_slice(&(size as i32).to_le_bytes());
            data.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // XOR + AND masks
            data.extend_from_slice(&1u16.to_le_bytes()); // planes
            data.extend_from_slice(&32u16.to_le_bytes()); // bpp
            data.extend_from_slice(&0u32.to_le_bytes()); // compression
            data.extend_from_slice(&((pixels.len() + and_mask.len()) as u32).to_le_bytes());
            data.extend_from_slice(&[0u8; 16]); // resolution, colors
            data.extend_from_slice(&pixels);
            data.extend_from_slice(&and_mask);
            (size, data)
        })
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, data) in &images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in images {
        out.extend_from_slice(&data);
    }
    out
}

fn encode_png(name: IconName, size: u32) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, size, size);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&render_rgba(name, size))?;
    }
    Ok(out)
}

/// Write idle/recording/processing/paused .ico (and 256 px .png) files into `dir` and return it.
pub fn write_icons(dir: &Path) -> PathBuf {
    if let Err(e) = std::fs::create_dir_all(dir) {
        crate::warn!("[icons] could not create {}: {e}", dir.display());
    }
    for name in IconName::ALL {
        let _ = std::fs::write(dir.join(format!("{}.ico", name.as_str())), encode_ico(name));
        if let Ok(png) = encode_png(name, 256) {
            let _ = std::fs::write(dir.join(format!("{}.png", name.as_str())), png);
        }
    }
    dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ico_has_the_same_size_as_the_original() {
        // The TypeScript version produced 43,006-byte files.
        assert_eq!(encode_ico(IconName::Idle).len(), 43_006);
    }
}
