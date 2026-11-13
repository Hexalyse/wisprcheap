//! Edge glow (`overlay.style: edgeGlow`): a soft blue light rising from the bottom of the active window while
//! wisprcheap listens, and climbing its side edges. Blobs of light drift along the bottom, and the glow swells with
//! the voice; it breathes while the audio is processed, and briefly takes the outcome's tint.
//!
//! This is the platform-independent part (what to show, its colors, brightness and motion); the GTK4 window
//! places it over the active window and draws `GlowFrame`s with GSK gradients, on the GPU.

use std::f32::consts::TAU;
use std::time::Instant;

use super::Feedback;
use super::OverlayStatus;
use super::paint::Color;
use super::scene::loudness;
use crate::hotkey::Mode;

const SHOW_SECS: f32 = 0.25;
const HIDE_SECS: f32 = 0.45;
/// Breathing period while processing.
const BREATHE_SECS: f32 = 1.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlowLook {
    Hidden,
    Dictation,
    Command,
    Processing,
    Feedback(Feedback),
}

impl GlowLook {
    /// Three tints the blobs, rim and edges are drawn with.
    fn palette(self) -> [Color; 3] {
        let hex = |a, b, c| [Color::hex(a), Color::hex(b), Color::hex(c)];
        match self {
            // periwinkle blues
            GlowLook::Hidden | GlowLook::Dictation => hex(0x7C9CFF, 0x5B7CFA, 0xA3B8FF),
            GlowLook::Command => hex(0xA78BFA, 0x8B5CF6, 0xC4B5FD),
            GlowLook::Processing => hex(0x8FA8FF, 0x7C83F7, 0x9CC2FF),
            GlowLook::Feedback(Feedback::Pasted) => hex(0x6EE7F9, 0x60A5FA, 0x5EEAD4),
            GlowLook::Feedback(Feedback::Copied) => hex(0xBFD3FF, 0x93B4FD, 0xDCE6FF),
            GlowLook::Feedback(Feedback::Discarded) => hex(0x9CA3AF, 0x6B7280, 0xB4BAC4),
            GlowLook::Feedback(Feedback::Error) => hex(0xF87171, 0xEF4444, 0xFB7185),
        }
    }

    /// How fast the blobs drift (relative).
    fn speed(self) -> f32 {
        match self {
            GlowLook::Processing => 1.8,
            _ => 1.0,
        }
    }
}

/// Everything needed to draw one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlowFrame {
    /// Overall opacity, 0..1.
    pub opacity: f32,
    pub colors: [Color; 3],
    /// Motion clock, in seconds (runs faster while processing).
    pub time: f32,
    /// How far the glow reaches, 0..1 (the voice while recording).
    pub swell: f32,
}

pub struct GlowScene {
    status: OverlayStatus,
    /// Outcome being shown, until the instant.
    feedback: Option<(Feedback, Instant)>,
    /// Look on screen, kept while it fades out.
    look: GlowLook,
    /// 0 = hidden, 1 = fully shown.
    shown: f32,
    /// Smoothed voice loudness, 0..1.
    voice: f32,
    swell: f32,
    colors: [Color; 3],
    time: f32,
    breath: f32,
    last_tick: Option<Instant>,
}

impl Default for GlowScene {
    fn default() -> Self {
        Self::new()
    }
}

fn approach(value: f32, target: f32, dt: f32, tau: f32) -> f32 {
    value + (target - value) * (1.0 - (-dt / tau).exp())
}

impl GlowScene {
    pub fn new() -> Self {
        Self {
            status: OverlayStatus::default(),
            feedback: None,
            look: GlowLook::Hidden,
            shown: 0.0,
            voice: 0.0,
            swell: 0.0,
            colors: GlowLook::Dictation.palette(),
            time: 0.0,
            breath: 0.0,
            last_tick: None,
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

    fn target(&self, now: Instant) -> GlowLook {
        if !self.status.enabled {
            return GlowLook::Hidden;
        }
        if let Some(r) = self.status.recording {
            return match r.mode {
                Mode::Dictation => GlowLook::Dictation,
                Mode::Command => GlowLook::Command,
            };
        }
        match self.feedback {
            Some((f, until)) if now < until => GlowLook::Feedback(f),
            _ if self.status.busy => GlowLook::Processing,
            _ => GlowLook::Hidden,
        }
    }

    /// Whether the glow should be on screen at `now` (showing something, or still fading out).
    pub fn on_screen(&self, now: Instant) -> bool {
        self.shown > 0.0 || self.target(now) != GlowLook::Hidden
    }

    /// Advance to `now`. `level` is the loudest microphone RMS since the previous call.
    pub fn tick(&mut self, now: Instant, level: f32) {
        let asleep = self.shown == 0.0 && self.look == GlowLook::Hidden;
        let dt = match self.last_tick {
            Some(t) if !asleep => now.saturating_duration_since(t).as_secs_f32().min(0.05),
            _ => 0.0,
        };
        self.last_tick = Some(now);
        if self.feedback.is_some_and(|(_, until)| now >= until) {
            self.feedback = None;
        }
        let target = self.target(now);
        if target == GlowLook::Hidden {
            self.shown = (self.shown - dt / HIDE_SECS).max(0.0);
            if self.shown == 0.0 {
                self.look = GlowLook::Hidden;
            }
        } else {
            if self.look == GlowLook::Hidden {
                // appearing from nothing: start in the final colors, from silence
                self.voice = 0.0;
                self.swell = 0.0;
                self.colors = target.palette();
                self.breath = 0.0;
            }
            self.look = target;
            self.shown = (self.shown + dt / SHOW_SECS).min(1.0);
        }

        let heard = match self.look {
            GlowLook::Dictation | GlowLook::Command => loudness(level),
            _ => 0.0,
        };
        // Rise quickly with the voice, settle slowly between words.
        let tau = if heard > self.voice { 0.05 } else { 0.3 };
        self.voice = approach(self.voice, heard, dt, tau);
        let swell = match self.look {
            GlowLook::Dictation | GlowLook::Command => self.voice,
            GlowLook::Processing => 0.25,
            GlowLook::Feedback(_) => 0.6,
            GlowLook::Hidden => self.swell,
        };
        self.swell = approach(self.swell, swell, dt, 0.08);
        let palette = self.look.palette();
        for (color, target) in self.colors.iter_mut().zip(palette) {
            color.r = approach(color.r, target.r, dt, 0.12);
            color.g = approach(color.g, target.g, dt, 0.12);
            color.b = approach(color.b, target.b, dt, 0.12);
        }
        self.time += dt * self.look.speed();
        self.breath = (self.breath + dt / BREATHE_SECS).fract();
    }

    #[cfg(test)]
    pub fn look(&self) -> GlowLook {
        self.look
    }

    pub fn frame(&self) -> GlowFrame {
        let intensity = match self.look {
            GlowLook::Dictation | GlowLook::Command => 0.75 + 0.25 * self.voice,
            GlowLook::Processing => 0.7 + 0.3 * (TAU * self.breath).cos(),
            _ => 1.0,
        };
        // ease out cubic: appears quickly, fades out gently
        let shown = 1.0 - (1.0 - self.shown).powi(3);
        GlowFrame {
            opacity: shown * intensity,
            colors: self.colors,
            time: self.time,
            swell: self.swell,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::Recording;
    use std::time::Duration;

    const FRAME: Duration = Duration::from_millis(16);

    fn status(mode: Option<Mode>, busy: bool) -> OverlayStatus {
        OverlayStatus {
            enabled: true,
            recording: mode.map(|mode| Recording {
                mode,
                hands_free: false,
            }),
            busy,
            ..Default::default()
        }
    }

    /// Tick for `ms` at 60 fps with a constant microphone level.
    fn run(scene: &mut GlowScene, now: &mut Instant, ms: u64, level: f32) {
        let end = *now + Duration::from_millis(ms);
        while *now < end {
            *now += FRAME;
            scene.tick(*now, level);
        }
    }

    #[test]
    fn hidden_until_something_happens() {
        let mut now = Instant::now();
        let mut scene = GlowScene::new();
        scene.set_status(status(None, false));
        run(&mut scene, &mut now, 500, 0.0);
        assert!(!scene.on_screen(now));
        assert_eq!(scene.look(), GlowLook::Hidden);
        assert_eq!(scene.frame().opacity, 0.0);
    }

    #[test]
    fn disabled_shows_nothing() {
        let mut now = Instant::now();
        let mut scene = GlowScene::new();
        scene.set_status(OverlayStatus {
            enabled: false,
            ..status(Some(Mode::Dictation), true)
        });
        scene.feedback(Feedback::Error, now);
        assert!(!scene.on_screen(now));
        run(&mut scene, &mut now, 300, 0.5);
        assert_eq!(scene.frame().opacity, 0.0);
    }

    #[test]
    fn dictation_is_blue_and_swells_with_the_voice() {
        let mut now = Instant::now();
        let mut scene = GlowScene::new();
        scene.set_status(status(Some(Mode::Dictation), false));
        scene.tick(now, 0.0);
        assert!(scene.on_screen(now));
        assert_eq!(scene.look(), GlowLook::Dictation);
        let blue = scene.frame().colors[0];
        assert!(blue.b > blue.r && blue.b > blue.g, "{blue:?}");
        run(&mut scene, &mut now, 400, 0.0);
        let quiet = scene.frame();
        assert!(
            quiet.swell < 0.01 && (quiet.opacity - 0.75).abs() < 0.01,
            "{quiet:?}"
        );
        run(&mut scene, &mut now, 300, 0.3); // about -10 dBFS: loud
        let loud = scene.frame();
        assert!(loud.swell > 0.9 && loud.opacity > 0.95, "{loud:?}");
        // settles back between words, more slowly than it rose
        run(&mut scene, &mut now, 100, 0.0);
        assert!(scene.frame().swell > 0.5);
        run(&mut scene, &mut now, 2000, 0.0);
        assert!(scene.frame().swell < 0.01);
    }

    #[test]
    fn the_glow_keeps_moving() {
        let mut now = Instant::now();
        let mut scene = GlowScene::new();
        scene.set_status(status(Some(Mode::Dictation), false));
        run(&mut scene, &mut now, 200, 0.0);
        let before = scene.frame().time;
        run(&mut scene, &mut now, 500, 0.0);
        let recording = scene.frame().time - before;
        assert!((recording - 0.5).abs() < 0.05, "{recording}");
        // faster while processing
        scene.set_status(status(None, true));
        let before = scene.frame().time;
        run(&mut scene, &mut now, 500, 0.0);
        assert!(scene.frame().time - before > 0.85);
    }

    #[test]
    fn command_mode_has_its_own_tint() {
        let mut now = Instant::now();
        let mut scene = GlowScene::new();
        scene.set_status(status(Some(Mode::Command), false));
        run(&mut scene, &mut now, 100, 0.0);
        assert_eq!(scene.look(), GlowLook::Command);
        assert_eq!(scene.frame().colors, GlowLook::Command.palette());
    }

    #[test]
    fn processing_breathes_then_outcome_then_fades_out() {
        let mut now = Instant::now();
        let mut scene = GlowScene::new();
        scene.set_status(status(Some(Mode::Dictation), false));
        run(&mut scene, &mut now, 400, 0.1);
        scene.set_status(status(None, true));
        run(&mut scene, &mut now, 100, 0.0);
        assert_eq!(scene.look(), GlowLook::Processing);
        let (mut low, mut high) = (1.0f32, 0.0f32);
        for _ in 0..120 {
            run(&mut scene, &mut now, 16, 0.0);
            low = low.min(scene.frame().opacity);
            high = high.max(scene.frame().opacity);
        }
        assert!(low < 0.5 && high > 0.95, "{low} {high}");
        scene.set_status(status(None, false));
        scene.feedback(Feedback::Error, now);
        run(&mut scene, &mut now, 600, 0.0);
        assert_eq!(scene.look(), GlowLook::Feedback(Feedback::Error));
        let red = scene.frame().colors[1];
        assert!(red.r > red.b, "{red:?}");
        // the outcome lasts its duration, then the glow fades out keeping its tint
        // (600 ms of it already shown; 100 ms into the fade-out)
        run(
            &mut scene,
            &mut now,
            Feedback::Error.duration().as_millis() as u64 - 500,
            0.0,
        );
        assert_eq!(scene.look(), GlowLook::Feedback(Feedback::Error));
        assert!(scene.on_screen(now) && scene.frame().opacity < 1.0);
        run(&mut scene, &mut now, 600, 0.0);
        assert!(!scene.on_screen(now));
        assert_eq!(scene.look(), GlowLook::Hidden);
    }

    #[test]
    fn a_new_recording_replaces_the_outcome() {
        let mut now = Instant::now();
        let mut scene = GlowScene::new();
        scene.set_status(status(None, false));
        scene.feedback(Feedback::Error, now);
        run(&mut scene, &mut now, 100, 0.0);
        assert_eq!(scene.look(), GlowLook::Feedback(Feedback::Error));
        scene.set_status(status(Some(Mode::Dictation), false));
        run(&mut scene, &mut now, 50, 0.0);
        assert_eq!(scene.look(), GlowLook::Dictation);
    }
}
