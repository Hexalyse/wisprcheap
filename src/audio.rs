//! PCM helpers: WAV encoding, levels, channel mixing and resampling to 16 kHz.

pub const SAMPLE_RATE: u32 = 16_000;

pub fn pcm_duration_ms(samples: usize) -> f64 {
    samples as f64 / SAMPLE_RATE as f64 * 1000.0
}

/// Little-endian bytes of the samples.
pub fn pcm_bytes(pcm: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Wrap mono 16-bit 16 kHz PCM in a WAV container.
pub fn encode_wav(pcm: &[i16]) -> Vec<u8> {
    let data_size = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(&pcm_bytes(pcm));
    out
}

/// RMS level (dBFS) of the loudest 100 ms window. Used to skip recordings with no speech at all.
pub fn loudest_window_db(pcm: &[i16]) -> f64 {
    let window = (SAMPLE_RATE as usize * 100 / 1000).max(1);
    let mut loudest = 0.0f64;
    for chunk in pcm.chunks(window) {
        let sum: f64 = chunk
            .iter()
            .map(|&s| {
                let v = s as f64 / 32768.0;
                v * v
            })
            .sum();
        loudest = loudest.max((sum / chunk.len() as f64).sqrt());
    }
    if loudest > 0.0 {
        20.0 * loudest.log10()
    } else {
        f64::NEG_INFINITY
    }
}

pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect()
}

/// Band-limited resampling (windowed sinc, Blackman window, 8 zero crossings per side).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64; // output samples per input sample
    let cutoff = ratio.min(1.0) * 0.97; // fraction of the input Nyquist frequency
    let half_width = (8.0 / cutoff).ceil();
    const RES: f64 = 512.0; // table entries per input sample
    let table_len = (half_width * RES) as usize + 2;
    let table: Vec<f32> = (0..table_len)
        .map(|i| {
            let x = i as f64 / RES;
            if x > half_width {
                return 0.0;
            }
            let arg = std::f64::consts::PI * x * cutoff;
            let sinc = if arg == 0.0 { 1.0 } else { arg.sin() / arg };
            let w = x / half_width;
            let window = 0.42
                + 0.5 * (std::f64::consts::PI * w).cos()
                + 0.08 * (2.0 * std::f64::consts::PI * w).cos();
            (cutoff * sinc * window) as f32
        })
        .collect();

    let out_len = (input.len() as f64 * ratio).floor() as usize;
    let hw = half_width as isize;
    let len = input.len() as isize;
    let mut out = Vec::with_capacity(out_len);
    for n in 0..out_len {
        let t = n as f64 / ratio;
        let center = t.floor() as isize;
        let lo = (center - hw + 1).max(0);
        let hi = (center + hw).min(len - 1);
        let mut acc = 0.0f32;
        for k in lo..=hi {
            let pos = (k as f64 - t).abs() * RES;
            let i0 = pos as usize;
            if i0 + 1 >= table_len {
                continue;
            }
            let frac = (pos - i0 as f64) as f32;
            let w = table[i0] + (table[i0 + 1] - table[i0]) * frac;
            acc += input[k as usize] * w;
        }
        out.push(acc);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header() {
        let wav = encode_wav(&vec![0i16; 1600]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 3200);
    }

    #[test]
    fn levels() {
        assert_eq!(loudest_window_db(&vec![0i16; 1600]), f64::NEG_INFINITY);
        let tone: Vec<i16> = (0..1600)
            .map(|i| ((i as f64 / 3.0).sin() * 32767.0).round() as i16)
            .collect();
        assert!((loudest_window_db(&tone) + 3.0).abs() < 0.2);
    }

    #[test]
    fn resampling_keeps_length_and_level() {
        let input: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        let out = resample(&input, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        let peak = out[1000..15000].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
    }
}
