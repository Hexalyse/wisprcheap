//! What the overlay shows and how it moves: a platform-independent state machine with its animations, drawn
//! with `paint`. The window code calls `tick` and `draw` about 60 times a second while it's on screen.
//!
//! The looks follow the Android app's bubble and hands-free toolbar:
//! - recording: a dark pill with a red disc (white mic) that swells with the voice and a scrolling waveform;
//!   indigo with sparkles in command mode, plus a padlock in hands-free mode
//! - processing: the pill shrinks to a circle with a spinner
//! - then briefly: a check (pasted), a clipboard (only copied), a crossed-out mic (nothing heard)
//!   or a shaking "!" (error)

use std::collections::VecDeque;
use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use super::paint::{self, Canvas, Color, Glyph, around};
use super::{Feedback, OverlayStatus};
use crate::hotkey::Mode;

/// Size of the window content in device-independent pixels. The pill sits in the middle, with room around
/// it for the shadow and the animations.
pub const CANVAS_W: f32 = 200.0;
pub const CANVAS_H: f32 = 72.0;

const PILL_H: f32 = 36.0;
const COMPACT_W: f32 = 36.0;
const RECORDING_W: f32 = 128.0;
const HANDS_FREE_W: f32 = 148.0;
/// Accent disc at the left of the recording pill.
const DISC: f32 = 26.0;
const DISC_INSET: f32 = (PILL_H - DISC) / 2.0;
/// Icons fill this share of their circle's diameter (their 32-unit design box, that is).
const ICON_BOX: f32 = 0.8;
const WAVE_GAP: f32 = 10.0;
const WAVE_W: f32 = 73.0;
const WAVE_H: f32 = 18.0;
const BARS: usize = 18;
/// A new waveform bar this often (about 1.4 s of history on screen).
const BAR_EVERY: Duration = Duration::from_millis(80);
const LOCK_SIZE: f32 = 18.0;

const SHOW_SECS: f32 = 0.2;
const HIDE_SECS: f32 = 0.16;

// A dark Material 3 palette around the brand indigo (#5B5BD6), plus the Android app's recording red.
const SURFACE: Color = Color::hex(0x2A2931);
const RIM: Color = Color::hex(0xFFFFFF).with_alpha(0.08);
const SHADOW: Color = Color::hex(0x000000).with_alpha(0.4);
const RECORD: Color = Color::hex(0xE5484D);
const COMMAND: Color = Color::hex(0x6E6ADE);
const PRIMARY: Color = Color::hex(0xB1A9FF);
const ON_PRIMARY: Color = Color::hex(0x221E6E);
const SECONDARY: Color = Color::hex(0xC6C4DD);
const ON_SECONDARY: Color = Color::hex(0x2F2F42);
const ERROR: Color = Color::hex(0xFFB4AB);
const ON_ERROR: Color = Color::hex(0x690005);
const ON_SURFACE_VARIANT: Color = Color::hex(0xC8C5D0);
const WHITE: Color = Color::hex(0xFFFFFF);

impl Feedback {
    fn duration(self) -> Duration {
        Duration::from_millis(match self {
            Feedback::Pasted => 700,
            Feedback::Copied => 1200,
            Feedback::Discarded => 900,
            Feedback::Error => 2000,
        })
    }

    /// Background, icon color, icon.
    fn look(self) -> (Color, Color, Glyph) {
        match self {
            Feedback::Pasted => (PRIMARY, ON_PRIMARY, Glyph::Check),
            Feedback::Copied => (SECONDARY, ON_SECONDARY, Glyph::Clipboard),
            Feedback::Discarded => (SURFACE, PRIMARY, Glyph::MicOff),
            Feedback::Error => (ERROR, ON_ERROR, Glyph::Alert),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Look {
    Hidden,
    Recording { command: bool, hands_free: bool },
    Processing,
    Feedback(Feedback),
}

/// Where the animated values are heading for a look.
#[derive(Debug, Clone, Copy)]
struct Targets {
    width: f32,
    bg: Color,
    accent: Color,
    recording: f32,
    spinner: f32,
    icon: f32,
    lock: f32,
}

impl Targets {
    fn of(look: Look) -> Option<Self> {
        let base = Targets {
            width: COMPACT_W,
            bg: SURFACE,
            accent: RECORD,
            recording: 0.0,
            spinner: 0.0,
            icon: 0.0,
            lock: 0.0,
        };
        Some(match look {
            Look::Hidden => return None,
            Look::Recording {
                command,
                hands_free,
            } => Targets {
                width: if hands_free {
                    HANDS_FREE_W
                } else {
                    RECORDING_W
                },
                accent: if command { COMMAND } else { RECORD },
                recording: 1.0,
                lock: if hands_free { 1.0 } else { 0.0 },
                ..base
            },
            Look::Processing => Targets {
                spinner: 1.0,
                ..base
            },
            Look::Feedback(f) => Targets {
                bg: f.look().0,
                icon: 1.0,
                ..base
            },
        })
    }
}

/// Damped spring (same model as Compose's `spring()`), integrated in small steps.
#[derive(Debug, Clone, Copy)]
struct Spring {
    x: f32,
    v: f32,
}

impl Spring {
    const fn at(x: f32) -> Self {
        Self { x, v: 0.0 }
    }

    fn step(&mut self, target: f32, stiffness: f32, damping_ratio: f32, dt: f32) {
        let damping = 2.0 * damping_ratio * stiffness.sqrt();
        let steps = (dt * 240.0).ceil().max(1.0);
        let h = dt / steps;
        for _ in 0..steps as usize {
            let a = -stiffness * (self.x - target) - damping * self.v;
            self.v += a * h;
            self.x += self.v * h;
        }
        // settle exactly, so the cached backdrop stops changing
        if (self.x - target).abs() < 0.01 && self.v.abs() < 0.05 {
            *self = Self::at(target);
        }
    }
}

/// Exponential approach: about 63% of the way in `tau` seconds (and exactly there at the end).
fn approach(value: f32, target: f32, dt: f32, tau: f32) -> f32 {
    let next = value + (target - value) * (1.0 - (-dt / tau).exp());
    if (next - target).abs() < 0.002 {
        target
    } else {
        next
    }
}

fn approach_color(value: Color, target: Color, dt: f32, tau: f32) -> Color {
    Color {
        r: approach(value.r, target.r, dt, tau),
        g: approach(value.g, target.g, dt, tau),
        b: approach(value.b, target.b, dt, tau),
        a: approach(value.a, target.a, dt, tau),
    }
}

/// Microphone RMS (linear) to 0..1, like the Android app: -60 dBFS is silence, -10 dBFS is loud.
fn loudness(rms: f32) -> f32 {
    let db = 20.0 * rms.max(1e-6).log10();
    ((db + 60.0) / 50.0).clamp(0.0, 1.0)
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

fn ease_out_back(t: f32) -> f32 {
    let (c1, c3) = (1.4, 2.4);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

/// Horizontal offset of the error shake (Android's keyframes, in dp).
fn shake_offset(ms: f32) -> f32 {
    const KEYS: [(f32, f32); 7] = [
        (0.0, 0.0),
        (60.0, 10.0),
        (120.0, -10.0),
        (180.0, 7.0),
        (240.0, -7.0),
        (300.0, 3.0),
        (360.0, 0.0),
    ];
    for pair in KEYS.windows(2) {
        let ((t0, v0), (t1, v1)) = (pair[0], pair[1]);
        if ms <= t1 {
            return v0 + (v1 - v0) * ((ms - t0) / (t1 - t0)).clamp(0.0, 1.0);
        }
    }
    0.0
}

/// Indeterminate spinner like Material's: an arc whose head and tail chase each other while it turns.
/// Returns (start angle, sweep) in radians.
fn spinner_arc(t: f32) -> (f32, f32) {
    const CYCLE: f32 = 1.333;
    const MIN: f32 = 0.05;
    const MAX: f32 = 0.75;
    let smooth = |u: f32| u * u * (3.0 - 2.0 * u);
    let cycles = t / CYCLE;
    let (k, phase) = (cycles.floor(), cycles.fract());
    let head = smooth((phase * 2.0).min(1.0));
    let tail = smooth((phase * 2.0 - 1.0).max(0.0));
    let grow = MAX - MIN;
    let start = (k + tail) * grow;
    let end = (k + head) * grow + MIN;
    let turn = t / 1.8;
    (
        TAU * (start + turn).fract() - FRAC_PI_2,
        TAU * (end - start),
    )
}

pub struct Scene {
    status: OverlayStatus,
    /// Outcome being shown, until the instant.
    feedback: Option<(Feedback, Instant)>,
    look: Look,
    targets: Targets,
    last_tick: Option<Instant>,
    /// 0 = hidden, 1 = fully shown (appearing / disappearing).
    shown: f32,
    width: Spring,
    bg: Color,
    accent: Color,
    /// Opacity of each kind of content (they cross-fade).
    recording: f32,
    spinner: f32,
    icon: f32,
    lock: f32,
    /// Feedback whose icon is shown (kept while it fades out).
    icon_kind: Feedback,
    /// Command mode glyph on the disc.
    command: bool,
    /// Scale of the accent disc, following the voice.
    disc: Spring,
    /// Waveform, oldest first (0..1).
    bars: VecDeque<f32>,
    next_bar: Instant,
    /// Loudest input since the last bar (RMS).
    peak: f32,
    shake_from: Option<Instant>,
    clock: Instant,
    /// The shadow and the pill, reused while they don't change (most frames: only the content moves).
    backdrop: Option<(Backdrop, Vec<u8>)>,
}

/// Everything the backdrop's pixels depend on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Backdrop {
    size: (usize, usize),
    origin: (f32, f32),
    scale: f32,
    opacity: f32,
    half_width: f32,
    bg: Color,
}

impl Scene {
    pub fn new(now: Instant) -> Self {
        Self {
            status: OverlayStatus::default(),
            feedback: None,
            look: Look::Hidden,
            targets: Targets::of(Look::Processing).expect("targets"),
            last_tick: None,
            shown: 0.0,
            width: Spring::at(COMPACT_W),
            bg: SURFACE,
            accent: RECORD,
            recording: 0.0,
            spinner: 0.0,
            icon: 0.0,
            lock: 0.0,
            icon_kind: Feedback::Pasted,
            command: false,
            disc: Spring::at(1.0),
            bars: VecDeque::new(),
            next_bar: now,
            peak: 0.0,
            shake_from: None,
            clock: now,
            backdrop: None,
        }
    }

    pub fn set_status(&mut self, status: OverlayStatus) {
        if status.recording.is_some() && self.status.recording.is_none() {
            // a new recording replaces the previous outcome
            self.feedback = None;
        }
        self.status = status;
    }

    pub fn feedback(&mut self, feedback: Feedback, now: Instant) {
        self.feedback = Some((feedback, now + feedback.duration()));
    }

    fn target(&self, now: Instant) -> Look {
        if !self.status.enabled {
            return Look::Hidden;
        }
        if let Some(r) = self.status.recording {
            return Look::Recording {
                command: r.mode == Mode::Command,
                hands_free: r.hands_free,
            };
        }
        match self.feedback {
            Some((f, until)) if now < until => Look::Feedback(f),
            _ if self.status.busy => Look::Processing,
            _ => Look::Hidden,
        }
    }

    /// Whether the overlay should be on screen at `now` (showing something, or still fading out).
    pub fn on_screen(&self, now: Instant) -> bool {
        self.shown > 0.0 || self.target(now) != Look::Hidden
    }

    /// Advance the animations to `now`. `level` is the loudest microphone RMS since the previous call.
    pub fn tick(&mut self, now: Instant, level: f32) {
        // Nothing ticks while hidden: start the next appearance from its first frame.
        let asleep = self.shown == 0.0 && self.look == Look::Hidden;
        let dt = match self.last_tick {
            Some(t) if !asleep => now.saturating_duration_since(t).as_secs_f32().min(0.05),
            _ => 0.0,
        };
        self.last_tick = Some(now);
        if self.feedback.is_some_and(|(_, until)| now >= until) {
            self.feedback = None;
        }

        let look = self.target(now);
        if look != self.look {
            self.enter(look, now);
        }

        if look == Look::Hidden {
            self.shown = (self.shown - dt / HIDE_SECS).max(0.0);
        } else {
            self.shown = (self.shown + dt / SHOW_SECS).min(1.0);
        }
        if let Look::Recording { command, .. } = look {
            self.command = command;
            self.listen(now, level);
        }

        let t = self.targets;
        self.width.step(t.width, 500.0, 0.72, dt);
        self.bg = approach_color(self.bg, t.bg, dt, 0.05);
        self.accent = approach_color(self.accent, t.accent, dt, 0.05);
        self.recording = approach(self.recording, t.recording, dt, 0.05);
        self.spinner = approach(self.spinner, t.spinner, dt, 0.06);
        self.icon = approach(self.icon, t.icon, dt, 0.06);
        self.lock = approach(self.lock, t.lock, dt, 0.08);
        let voice = match look {
            Look::Recording { .. } => self.bars.back().copied().unwrap_or(0.0),
            _ => 0.0,
        };
        self.disc.step(1.0 + 0.16 * voice, 400.0, 0.55, dt);
    }

    fn enter(&mut self, look: Look, now: Instant) {
        let was_recording = matches!(self.look, Look::Recording { .. });
        if let Some(targets) = Targets::of(look) {
            self.targets = targets;
            if self.shown == 0.0 {
                // Appearing from nothing: start from a circle in the final colors, no cross-fades.
                self.width = Spring::at(COMPACT_W);
                self.bg = targets.bg;
                self.accent = targets.accent;
                self.recording = targets.recording;
                self.spinner = targets.spinner;
                self.icon = targets.icon;
                self.lock = targets.lock;
                self.disc = Spring::at(1.0);
                self.shake_from = None;
            }
        }
        if let Look::Recording { .. } = look
            && !was_recording
        {
            self.bars = std::iter::repeat_n(0.0, BARS + 1).collect();
            self.next_bar = now + BAR_EVERY;
            self.peak = 0.0;
        }
        if let Look::Feedback(f) = look {
            self.icon_kind = f;
            if f == Feedback::Error {
                self.shake_from = Some(now);
            }
        }
        if look == Look::Processing {
            self.clock = now;
        }
        self.look = look;
    }

    fn listen(&mut self, now: Instant, level: f32) {
        if level.is_finite() {
            self.peak = self.peak.max(level);
        }
        if now < self.next_bar {
            return;
        }
        self.bars.push_back(loudness(self.peak));
        while self.bars.len() > BARS + 1 {
            self.bars.pop_front();
        }
        self.peak = 0.0;
        self.next_bar += BAR_EVERY;
        if self.next_bar <= now {
            self.next_bar = now + BAR_EVERY;
        }
    }

    /// Draw the current frame. `scale` is the number of pixels per device-independent pixel; the canvas
    /// must be `CANVAS_W` x `CANVAS_H` times that.
    pub fn draw(&mut self, canvas: &mut Canvas, scale: f32, now: Instant) {
        if self.shown <= 0.0 {
            canvas.clear();
            return;
        }
        let (opacity, zoom, lift) = if self.look == Look::Hidden {
            (self.shown, 0.85 + 0.15 * self.shown, 0.0)
        } else {
            let e = ease_out_cubic(self.shown);
            (e, 0.7 + 0.3 * ease_out_back(self.shown), (1.0 - e) * 8.0)
        };
        let shake = self.shake_from.map_or(0.0, |from| {
            shake_offset(now.saturating_duration_since(from).as_secs_f32() * 1000.0)
        });
        let origin = (
            (CANVAS_W / 2.0 + shake) * scale,
            (CANVAS_H / 2.0 + lift) * scale,
        );
        canvas.set_transform(origin, scale * zoom, opacity);

        let hw = self.width.x.max(PILL_H) / 2.0;
        let hh = PILL_H / 2.0;
        let pill = move |x: f32, y: f32| paint::round_box(x, y, 0.0, 0.0, hw, hh, hh);
        let backdrop = Backdrop {
            size: (canvas.width, canvas.height),
            origin,
            scale: scale * zoom,
            opacity,
            half_width: hw,
            bg: self.bg,
        };
        match &mut self.backdrop {
            Some((key, pixels)) if *key == backdrop => canvas.pixels.copy_from_slice(pixels),
            cache => {
                canvas.clear();
                let pill_bounds = around(0.0, 0.0, hw + 1.0, hh + 1.0);
                canvas.shadow(around(0.0, 3.0, hw + 9.0, hh + 9.0), SHADOW, 7.0, |x, y| {
                    pill(x, y - 3.0)
                });
                canvas.fill(pill_bounds, self.bg, pill);
                // a faint inner rim keeps the pill readable on dark backgrounds
                canvas.fill(pill_bounds, RIM, |x, y| {
                    let d = pill(x, y);
                    d.max(-d - 1.0)
                });
                let pixels = match cache.take() {
                    Some((_, mut pixels)) => {
                        pixels.clear();
                        pixels.extend_from_slice(&canvas.pixels);
                        pixels
                    }
                    None => canvas.pixels.clone(),
                };
                *cache = Some((backdrop, pixels));
            }
        }

        if self.recording > 0.01 {
            self.draw_recording(canvas, hw, now, &pill);
        }
        if self.spinner > 0.01 {
            let t = now.saturating_duration_since(self.clock).as_secs_f32();
            let (start, sweep) = spinner_arc(t);
            canvas.fill(
                around(0.0, 0.0, 10.0, 10.0),
                PRIMARY.with_alpha(self.spinner),
                |x, y| paint::arc(x, y, (0.0, 0.0), 7.5, start, sweep, 1.3),
            );
        }
        if self.icon > 0.01 {
            let (_, fg, glyph) = self.icon_kind.look();
            let size = COMPACT_W * ICON_BOX;
            canvas.fill(
                around(0.0, 0.0, size / 2.0, size / 2.0),
                fg.with_alpha(self.icon),
                |x, y| paint::glyph(glyph, x, y, 0.0, 0.0, size),
            );
        }
    }

    fn draw_recording(
        &self,
        canvas: &mut Canvas,
        hw: f32,
        now: Instant,
        pill: &impl Fn(f32, f32) -> f32,
    ) {
        let a = self.recording;
        // Laid out from the pill's left edge, so the disc slides from the center as the pill opens, and
        // clipped to the pill, which reveals the waveform.
        let disc_x = -hw + DISC_INSET + DISC / 2.0;
        let r = DISC / 2.0 * self.disc.x;
        canvas.fill_clipped(
            around(disc_x, 0.0, r + 1.0, r + 1.0),
            self.accent.with_alpha(a),
            |x, y| paint::circle(x, y, disc_x, 0.0, r),
            pill,
        );
        let glyph = if self.command {
            Glyph::Sparkle
        } else {
            Glyph::Mic
        };
        let size = DISC * ICON_BOX * self.disc.x;
        canvas.fill_clipped(
            around(disc_x, 0.0, size / 2.0, size / 2.0),
            WHITE.with_alpha(a),
            |x, y| paint::glyph(glyph, x, y, disc_x, 0.0, size),
            pill,
        );

        // Waveform: the newest bar enters on the right and every bar slides one slot left per BAR_EVERY.
        let left = disc_x + DISC / 2.0 + WAVE_GAP;
        let right = left + WAVE_W;
        let step = WAVE_W / BARS as f32;
        let bar_w = step * 0.55;
        let until_next = self.next_bar.saturating_duration_since(now).as_secs_f32();
        let progress = (1.0 - until_next / BAR_EVERY.as_secs_f32()).clamp(0.0, 1.0);
        let newest = self.bars.len().saturating_sub(1);
        for (i, &level) in self.bars.iter().enumerate() {
            let age = (newest - i) as f32;
            let x = right - (age + progress - 0.5) * step;
            let fade = ((x - left) / step).clamp(0.0, 1.0) * ((right - x) / step).clamp(0.0, 1.0);
            if fade <= 0.0 {
                continue;
            }
            let h = (level * WAVE_H).max(bar_w);
            let half = (h - bar_w) / 2.0;
            canvas.fill_clipped(
                around(x, 0.0, bar_w, h / 2.0 + 1.0),
                self.accent.with_alpha(a * fade),
                |px, py| paint::capsule(px, py, (x, -half), (x, half), bar_w / 2.0),
                pill,
            );
        }

        if self.lock > 0.01 {
            let lock_x = hw - 17.0;
            canvas.fill_clipped(
                around(lock_x, 0.0, LOCK_SIZE / 2.0, LOCK_SIZE / 2.0),
                ON_SURFACE_VARIANT.with_alpha(a * self.lock),
                |x, y| paint::glyph(Glyph::Lock, x, y, lock_x, 0.0, LOCK_SIZE),
                pill,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::Recording;

    const FRAME: Duration = Duration::from_millis(16);

    fn status(recording: Option<Recording>, busy: bool) -> OverlayStatus {
        OverlayStatus {
            enabled: true,
            recording,
            busy,
        }
    }

    fn dictation() -> Option<Recording> {
        Some(Recording {
            mode: Mode::Dictation,
            hands_free: false,
        })
    }

    /// Tick for `ms` milliseconds with a constant level; returns the new time.
    fn run(scene: &mut Scene, mut now: Instant, ms: u64, level: f32) -> Instant {
        let end = now + Duration::from_millis(ms);
        while now < end {
            now += FRAME;
            scene.tick(now, level);
        }
        now
    }

    #[test]
    fn shows_while_recording_and_hides_after_a_cancel() {
        let t0 = Instant::now();
        let mut scene = Scene::new(t0);
        scene.tick(t0, 0.0);
        assert!(!scene.on_screen(t0));

        scene.set_status(status(dictation(), false));
        assert!(scene.on_screen(t0));
        let now = run(&mut scene, t0, 500, 0.1);
        assert_eq!(scene.shown, 1.0);
        assert!(scene.bars.iter().any(|&b| b > 0.5));
        assert!(scene.width.x > RECORDING_W - 2.0);

        scene.set_status(status(None, false));
        let now = run(&mut scene, now, 300, 0.0);
        assert!(!scene.on_screen(now));
    }

    #[test]
    fn processing_then_feedback_then_hidden() {
        let t0 = Instant::now();
        let mut scene = Scene::new(t0);
        scene.set_status(status(dictation(), false));
        let now = run(&mut scene, t0, 300, 0.0);
        scene.set_status(status(None, true));
        let now = run(&mut scene, now, 100, 0.0);
        assert_eq!(scene.look, Look::Processing);

        scene.feedback(Feedback::Pasted, now);
        let now = run(&mut scene, now, 50, 0.0);
        assert_eq!(scene.look, Look::Feedback(Feedback::Pasted));
        scene.set_status(status(None, false));
        let now = run(&mut scene, now, 600, 0.0);
        assert!(scene.on_screen(now), "the check stays a moment");
        let now = run(&mut scene, now, 400, 0.0);
        assert!(!scene.on_screen(now));
    }

    #[test]
    fn feedback_returns_to_processing_while_busy_and_a_new_recording_clears_it() {
        let t0 = Instant::now();
        let mut scene = Scene::new(t0);
        scene.set_status(status(None, true));
        scene.feedback(Feedback::Error, t0);
        let now = run(&mut scene, t0, 100, 0.0);
        assert_eq!(scene.look, Look::Feedback(Feedback::Error));
        let now = run(&mut scene, now, 2000, 0.0);
        assert_eq!(scene.look, Look::Processing);

        scene.feedback(Feedback::Discarded, now);
        scene.set_status(status(dictation(), true));
        let now = run(&mut scene, now, 50, 0.0);
        assert!(matches!(scene.look, Look::Recording { .. }));
        scene.set_status(status(None, true));
        run(&mut scene, now, 50, 0.0);
        assert_eq!(scene.look, Look::Processing);
    }

    #[test]
    fn disabled_shows_nothing() {
        let t0 = Instant::now();
        let mut scene = Scene::new(t0);
        scene.set_status(OverlayStatus {
            enabled: false,
            recording: dictation(),
            busy: true,
        });
        scene.feedback(Feedback::Error, t0);
        assert!(!scene.on_screen(t0));
    }

    #[test]
    fn loudness_maps_like_android() {
        assert_eq!(loudness(0.0), 0.0);
        assert!((loudness(10f32.powf(-35.0 / 20.0)) - 0.5).abs() < 1e-4);
        assert_eq!(loudness(1.0), 1.0);
    }

    #[test]
    fn draws_an_opaque_pill() {
        let t0 = Instant::now();
        let mut scene = Scene::new(t0);
        scene.set_status(status(dictation(), false));
        let now = run(&mut scene, t0, 600, 0.05);
        let scale = 1.5;
        let (w, h) = (
            (CANVAS_W * scale).ceil() as usize,
            (CANVAS_H * scale).ceil() as usize,
        );
        let mut canvas = Canvas::new(w, h);
        scene.draw(&mut canvas, scale, now);
        let alpha = |x: usize, y: usize| canvas.pixels[(y * w + x) * 4 + 3];
        // between the disc and the waveform, inside the pill
        let (cx, cy) = (w / 2, h / 2);
        assert_eq!(alpha(cx - 20, cy - 15), 255);
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(w - 1, h - 1), 0);
    }

    /// Puts a scene in some state; returns the time it got to.
    type Setup = dyn Fn(&mut Scene, Instant) -> Instant;

    #[test]
    fn the_cached_backdrop_draws_the_same_pixels() {
        let t0 = Instant::now();
        let mut scene = Scene::new(t0);
        let hands_free = Some(Recording {
            mode: Mode::Dictation,
            hands_free: true,
        });
        scene.set_status(status(hands_free, false));
        let now = run(&mut scene, t0, 1000, 0.05);
        let scale = 1.25;
        let size = (
            (CANVAS_W * scale).ceil() as usize,
            (CANVAS_H * scale).ceil() as usize,
        );
        let mut first = Canvas::new(size.0, size.1);
        scene.draw(&mut first, scale, now);
        let mut cached = Canvas::new(size.0, size.1);
        scene.draw(&mut cached, scale, now);
        assert!(scene.backdrop.is_some());
        assert!(first.pixels == cached.pixels);
        // the animations have settled: the next frame reuses the backdrop too
        let key = scene.backdrop.as_ref().map(|(k, _)| *k);
        let now = run(&mut scene, now, 50, 0.05);
        scene.draw(&mut cached, scale, now);
        assert_eq!(scene.backdrop.as_ref().map(|(k, _)| *k), key);
    }

    /// `cargo test overlay_previews -- --ignored` writes PNGs of every look to target/overlay-preview.
    #[test]
    #[ignore]
    fn overlay_previews() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/overlay-preview");
        std::fs::create_dir_all(&dir).unwrap();
        let scale = 2.0;
        let (w, h) = (
            (CANVAS_W * scale).ceil() as usize,
            (CANVAS_H * scale).ceil() as usize,
        );
        let shots: Vec<(&str, Box<Setup>)> = vec![
            (
                "1-recording",
                Box::new(|s, t| {
                    s.set_status(status(dictation(), false));
                    let mut now = t;
                    for i in 0..40 {
                        let level =
                            10f32.powf((-50.0 + 40.0 * ((i as f32) * 0.7).sin().abs()) / 20.0);
                        now = run(s, now, 55, level);
                    }
                    now
                }),
            ),
            (
                "2-command",
                Box::new(|s, t| {
                    s.set_status(status(
                        Some(Recording {
                            mode: Mode::Command,
                            hands_free: false,
                        }),
                        false,
                    ));
                    let mut now = t;
                    for i in 0..40 {
                        let level =
                            10f32.powf((-45.0 + 30.0 * ((i as f32) * 1.3).cos().abs()) / 20.0);
                        now = run(s, now, 55, level);
                    }
                    now
                }),
            ),
            (
                "3-hands-free",
                Box::new(|s, t| {
                    s.set_status(status(
                        Some(Recording {
                            mode: Mode::Dictation,
                            hands_free: true,
                        }),
                        false,
                    ));
                    let mut now = t;
                    for i in 0..40 {
                        let level = 10f32.powf((-40.0 + 25.0 * ((i as f32) * 0.4).sin()) / 20.0);
                        now = run(s, now, 55, level);
                    }
                    now
                }),
            ),
            (
                "4-processing",
                Box::new(|s, t| {
                    s.set_status(status(None, true));
                    run(s, t, 900, 0.0)
                }),
            ),
            (
                "5-pasted",
                Box::new(|s, t| {
                    s.feedback(Feedback::Pasted, t);
                    run(s, t, 400, 0.0)
                }),
            ),
            (
                "6-copied",
                Box::new(|s, t| {
                    s.feedback(Feedback::Copied, t);
                    run(s, t, 400, 0.0)
                }),
            ),
            (
                "7-discarded",
                Box::new(|s, t| {
                    s.feedback(Feedback::Discarded, t);
                    run(s, t, 400, 0.0)
                }),
            ),
            (
                "8-error",
                Box::new(|s, t| {
                    s.feedback(Feedback::Error, t);
                    run(s, t, 500, 0.0)
                }),
            ),
        ];
        for (name, setup) in shots {
            let t0 = Instant::now();
            let mut scene = Scene::new(t0);
            scene.set_status(status(None, false));
            let now = setup(&mut scene, t0);
            let mut canvas = Canvas::new(w, h);
            scene.draw(&mut canvas, scale, now);
            // over a light and a dark backdrop
            for (suffix, backdrop) in [("light", 0xF3F3F3u32), ("dark", 0x1B1B1Fu32)] {
                let bg = Color::hex(backdrop);
                let mut rgba = Vec::with_capacity(w * h * 4);
                for px in canvas.pixels.chunks(4) {
                    let a = px[3] as f32 / 255.0;
                    let mix = |c: u8, b: f32| (c as f32 + b * 255.0 * (1.0 - a)).round() as u8;
                    rgba.extend_from_slice(&[
                        mix(px[2], bg.r),
                        mix(px[1], bg.g),
                        mix(px[0], bg.b),
                        255,
                    ]);
                }
                let file = std::fs::File::create(dir.join(format!("{name}-{suffix}.png"))).unwrap();
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
    }
}
