//! Sound cues: short sine tones synthesized at startup, played on the default output device.
//! The output stream is opened for each cue (so the current default device is always used) and
//! closed shortly after the cue ends.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::audio::resample;
use crate::config::SoundsConfig;

const RATE: u32 = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cue {
    Start,
    Stop,
    Lock,
    /// Switched to command mode during the hold.
    Command,
    /// A word was added to the dictionary.
    Added,
    Cancel,
    Error,
}

/// (frequency Hz, 0 = silence; duration ms)
fn notes(cue: Cue) -> &'static [(f64, u32)] {
    match cue {
        Cue::Start => &[(587.0, 55), (880.0, 75)],
        Cue::Stop => &[(880.0, 55), (587.0, 75)],
        Cue::Lock => &[(880.0, 45), (0.0, 25), (880.0, 45), (0.0, 25), (1175.0, 70)],
        Cue::Command => &[(659.0, 50), (988.0, 50), (1319.0, 80)],
        Cue::Added => &[(1047.0, 60), (0.0, 30), (1568.0, 90)],
        Cue::Cancel => &[(440.0, 80)],
        Cue::Error => &[(233.0, 140), (0.0, 60), (175.0, 220)],
    }
}

const ALL_CUES: [Cue; 7] = [
    Cue::Start,
    Cue::Stop,
    Cue::Lock,
    Cue::Command,
    Cue::Added,
    Cue::Cancel,
    Cue::Error,
];

fn synth(notes: &[(f64, u32)], volume: f64) -> Vec<f32> {
    let len_of = |ms: u32| (RATE as f64 * ms as f64 / 1000.0).round() as usize;
    let total: usize = notes.iter().map(|&(_, ms)| len_of(ms)).sum();
    let mut out = vec![0.0f32; total];
    let fade = (RATE as f64 * 0.006).round(); // 6 ms fade in/out to avoid clicks
    let mut offset = 0;
    for &(freq, ms) in notes {
        let len = len_of(ms);
        if freq > 0.0 {
            for i in 0..len {
                let envelope = 1f64
                    .min(i as f64 / fade)
                    .min((len as f64 - 1.0 - i as f64) / fade);
                let v = (2.0 * std::f64::consts::PI * freq * i as f64 / RATE as f64).sin()
                    * envelope
                    * volume;
                // Quantize like the 16-bit original.
                out[offset + i] = ((v * 32767.0).round() / 32767.0) as f32;
            }
        }
        offset += len;
    }
    out
}

enum Msg {
    Play(Arc<Vec<f32>>),
    Quit,
}

pub struct Sounds {
    enabled: bool,
    buffers: HashMap<Cue, Arc<Vec<f32>>>,
    tx: Option<mpsc::Sender<Msg>>,
}

fn build_output<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    samples: Vec<f32>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels.max(1) as usize;
    let mut pos = 0usize;
    let stream = device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            for frame in data.chunks_mut(channels) {
                let v = samples.get(pos).copied().unwrap_or(0.0);
                pos += 1;
                for s in frame {
                    *s = T::from_sample(v);
                }
            }
        },
        |e| crate::warn!("[sounds] playback failed: {e}"),
        None,
    )?;
    Ok(stream)
}

fn open_output(samples: &[f32]) -> Result<(cpal::Stream, Duration)> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| anyhow!("no audio output available"))?;
    let supported = device.default_output_config()?;
    let rate = supported.sample_rate();
    let data = resample(samples, RATE, rate);
    let duration = Duration::from_secs_f64(data.len() as f64 / rate.max(1) as f64);
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_output::<f32>(&device, config, data)?,
        SampleFormat::I16 => build_output::<i16>(&device, config, data)?,
        SampleFormat::U16 => build_output::<u16>(&device, config, data)?,
        SampleFormat::I32 => build_output::<i32>(&device, config, data)?,
        SampleFormat::U32 => build_output::<u32>(&device, config, data)?,
        SampleFormat::I8 => build_output::<i8>(&device, config, data)?,
        SampleFormat::U8 => build_output::<u8>(&device, config, data)?,
        SampleFormat::F64 => build_output::<f64>(&device, config, data)?,
        other => bail!("unsupported output sample format {other:?}"),
    };
    stream.play()?;
    Ok((stream, duration))
}

fn player(rx: mpsc::Receiver<Msg>) {
    let mut current: Option<cpal::Stream> = None;
    let mut deadline: Option<Instant> = None;
    loop {
        let msg = match deadline {
            Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match msg {
            Ok(Msg::Play(samples)) => {
                current = None; // cut off a cue that's still playing
                match open_output(&samples) {
                    Ok((stream, duration)) => {
                        current = Some(stream);
                        deadline = Some(Instant::now() + duration + Duration::from_millis(120));
                    }
                    Err(e) => {
                        crate::warn!("[sounds] playback failed: {e}");
                        deadline = None;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                current = None;
                deadline = None;
            }
            Ok(Msg::Quit) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(current);
}

impl Sounds {
    pub fn new(opts: &SoundsConfig) -> Self {
        let enabled = opts.enabled && opts.volume > 0.0;
        let buffers = ALL_CUES
            .iter()
            .map(|&cue| (cue, Arc::new(synth(notes(cue), opts.volume))))
            .collect();
        let tx = enabled.then(|| {
            let (tx, rx) = mpsc::channel();
            let _ = std::thread::Builder::new()
                .name("sounds".into())
                .spawn(move || player(rx));
            tx
        });
        Self {
            enabled,
            buffers,
            tx,
        }
    }

    /// Fire-and-forget.
    pub fn play(&self, cue: Cue) {
        if !self.enabled {
            return;
        }
        if let (Some(tx), Some(buf)) = (&self.tx, self.buffers.get(&cue)) {
            let _ = tx.send(Msg::Play(buf.clone()));
        }
    }

    pub fn release(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Msg::Quit);
        }
    }

    /// Play a cue and wait until it has finished (CLI tests).
    pub fn play_blocking(&self, cue: Cue) {
        self.play(cue);
        let ms: u32 = notes(cue).iter().map(|&(_, ms)| ms).sum();
        std::thread::sleep(Duration::from_millis(ms as u64 + 150));
    }
}

impl Drop for Sounds {
    fn drop(&mut self) {
        self.release();
    }
}
