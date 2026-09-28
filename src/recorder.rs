//! Microphone capture with cpal. The device is opened for each recording and closed afterwards, so a
//! headset that was plugged in or a new default device is picked up without restarting. It also keeps
//! the mic (and the OS "microphone in use" indicator) off between dictations.
//!
//! Audio is captured in the device's native format, mixed to mono and resampled to 16 kHz int16.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};

use crate::audio::{SAMPLE_RATE, f32_to_i16, resample};
use crate::config::DeviceSetting;

pub fn list_input_devices() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

pub fn list_output_devices() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.map(|d| d.to_string()).collect())
        .unwrap_or_default()
}

fn default_input() -> Result<cpal::Device> {
    cpal::default_host()
        .default_input_device()
        .ok_or_else(|| anyhow!("no input device available"))
}

/// "default" / "" -> system default; a number -> that index; otherwise the first name containing the string.
pub fn resolve_device(setting: &DeviceSetting) -> Result<cpal::Device> {
    match setting {
        DeviceSetting::Index(i) if *i < 0 => default_input(),
        DeviceSetting::Index(i) => cpal::default_host()
            .input_devices()?
            .nth(*i as usize)
            .ok_or_else(|| anyhow!("no input device with index {i} (see `wisprcheap devices`)")),
        DeviceSetting::Name(name)
            if name.trim().is_empty() || name.eq_ignore_ascii_case("default") =>
        {
            default_input()
        }
        DeviceSetting::Name(name) => {
            let needle = name.to_lowercase();
            let devices: Vec<cpal::Device> = cpal::default_host().input_devices()?.collect();
            if let Some(d) = devices
                .iter()
                .find(|d| d.to_string().to_lowercase().contains(&needle))
            {
                return Ok(d.clone());
            }
            let names: Vec<String> = devices.iter().map(|d| d.to_string()).collect();
            crate::warn!(
                "[recorder] No input device matching \"{name}\", using the default. Available: {}",
                names.join(" | ")
            );
            default_input()
        }
    }
}

struct Captured {
    samples: Vec<f32>,
    rate: u32,
}

struct Active {
    stop_tx: mpsc::Sender<()>,
    thread: JoinHandle<Result<Captured>>,
}

/// A recording that was asked to stop: `finish()` (blocking) returns its 16 kHz samples.
pub struct StopHandle(Active);

impl StopHandle {
    pub fn finish(self) -> Vec<i16> {
        let _ = self.0.stop_tx.send(());
        match self.0.thread.join() {
            Ok(Ok(captured)) => {
                f32_to_i16(&resample(&captured.samples, captured.rate, SAMPLE_RATE))
            }
            Ok(Err(e)) => {
                crate::warn!("[recorder] read failed: {e}");
                Vec::new()
            }
            Err(_) => Vec::new(),
        }
    }
}

pub struct Recorder {
    device: DeviceSetting,
    active: Option<Active>,
    last_device_name: Option<String>,
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    buffer: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let stream = device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            let mut buf = buffer.lock().unwrap();
            for frame in data.chunks(channels) {
                let sum: f32 = frame.iter().map(|&s| f32::from_sample(s)).sum();
                buf.push(sum / frame.len() as f32);
            }
        },
        |e| crate::warn!("[recorder] read failed: {e}"),
        None,
    )?;
    Ok(stream)
}

fn open(device: &cpal::Device, buffer: Arc<Mutex<Vec<f32>>>) -> Result<(cpal::Stream, u32)> {
    let supported = device.default_input_config()?;
    let rate = supported.sample_rate();
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_stream::<f32>(device, config, buffer)?,
        SampleFormat::I16 => build_stream::<i16>(device, config, buffer)?,
        SampleFormat::U16 => build_stream::<u16>(device, config, buffer)?,
        SampleFormat::I32 => build_stream::<i32>(device, config, buffer)?,
        SampleFormat::U32 => build_stream::<u32>(device, config, buffer)?,
        SampleFormat::I8 => build_stream::<i8>(device, config, buffer)?,
        SampleFormat::U8 => build_stream::<u8>(device, config, buffer)?,
        SampleFormat::F64 => build_stream::<f64>(device, config, buffer)?,
        other => bail!("unsupported microphone sample format {other:?}"),
    };
    stream.play()?;
    Ok((stream, rate))
}

impl Recorder {
    pub fn new(device: DeviceSetting) -> Self {
        Self {
            device,
            active: None,
            last_device_name: None,
        }
    }

    /// Change the configured device (config reload). Applies from the next recording.
    pub fn set_device(&mut self, device: DeviceSetting) {
        self.device = device;
    }

    /// Name of the device a recording would use right now.
    pub fn current_device_name(&self) -> String {
        match resolve_device(&self.device) {
            Ok(d) => d.to_string(),
            Err(e) => format!("(unavailable: {e})"),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    pub fn start(&mut self) -> Result<()> {
        if self.active.is_some() {
            return Ok(());
        }
        let setting = self.device.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        // cpal streams can't move between threads on every platform: each recording gets its own thread.
        let thread = std::thread::Builder::new().name("recorder".into()).spawn(
            move || -> Result<Captured> {
                let buffer = Arc::new(Mutex::new(Vec::with_capacity(48_000 * 30)));
                let opened = resolve_device(&setting).and_then(|device| {
                    open(&device, buffer.clone()).map(|(s, r)| (device.to_string(), s, r))
                });
                let (name, stream, rate) = match opened {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = ready_tx.send(Err(anyhow!("{e}")));
                        return Err(e);
                    }
                };
                let _ = ready_tx.send(Ok(name));
                let _ = stop_rx.recv();
                drop(stream);
                let samples = std::mem::take(&mut *buffer.lock().unwrap());
                Ok(Captured { samples, rate })
            },
        )?;
        match ready_rx.recv() {
            Ok(Ok(name)) => {
                if self
                    .last_device_name
                    .as_ref()
                    .is_some_and(|last| *last != name)
                {
                    crate::say!("[recorder] Now using: {name}");
                }
                self.last_device_name = Some(name);
                self.active = Some(Active { stop_tx, thread });
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(anyhow!("the recorder thread exited"))
            }
        }
    }

    /// Stop recording. The returned handle gives everything captured since start().
    pub fn stop(&mut self) -> Option<StopHandle> {
        let active = self.active.take()?;
        let _ = active.stop_tx.send(());
        Some(StopHandle(active))
    }

    pub fn release(&mut self) {
        if let Some(handle) = self.stop() {
            drop(handle.finish());
        }
    }
}
