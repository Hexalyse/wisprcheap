//! Voice level for the overlays, relative to the background noise of the room and microphone.
//!
//! A fixed dBFS scale shows full bars all the time with a hot microphone (a laptop's internal mic with
//! PipeWire's boost, whose noise alone can reach -25 dBFS), and barely moves with a quiet one. So the noise
//! floor is tracked (the quietest moment of the last few seconds, remembered between recordings), and the
//! level is measured above it, up to just under full scale.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long the quietest reading counts as the noise floor.
const WINDOW: Duration = Duration::from_millis(2500);
/// Noise just above the floor still shows as silence.
const MARGIN_DB: f32 = 4.0;
/// From the floor to loud speech, at most (a quiet microphone) ...
const MAX_SPAN_DB: f32 = 36.0;
/// ... and at least (a hot one, with little headroom left).
const MIN_SPAN_DB: f32 = 18.0;
/// Loud speech, whatever the floor.
const LOUD_DB: f32 = -3.0;
/// Before anything was heard: a typical quiet microphone.
const DEFAULT_FLOOR_DB: f32 = -60.0;
const LOWEST_FLOOR_DB: f32 = -70.0;
const HIGHEST_FLOOR_DB: f32 = -12.0;

pub struct Meter {
    /// Recent readings (dBFS), oldest first.
    history: VecDeque<(Instant, f32)>,
    /// Floor of the previous recording, used until there's enough history.
    remembered: f32,
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

fn db(rms: f32) -> f32 {
    20.0 * rms.max(1e-6).log10()
}

impl Meter {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            remembered: DEFAULT_FLOOR_DB,
        }
    }

    /// Current noise floor estimate, in dBFS.
    pub fn floor(&self) -> f32 {
        let recent = self
            .history
            .iter()
            .map(|&(_, db)| db)
            .fold(f32::INFINITY, f32::min);
        let span = match (self.history.front(), self.history.back()) {
            (Some(&(first, _)), Some(&(last, _))) => last.saturating_duration_since(first),
            _ => Duration::ZERO,
        };
        // A recording often starts with speech: until a second has been heard, don't trust it alone.
        let floor = if span < Duration::from_secs(1) {
            recent.min(self.remembered)
        } else {
            recent
        };
        floor.clamp(LOWEST_FLOOR_DB, HIGHEST_FLOOR_DB)
    }

    /// Loudness 0..1 of a reading (linear RMS, > 0) taken at `now`.
    pub fn level(&mut self, rms: f32, now: Instant) -> f32 {
        let reading = db(rms);
        if self
            .history
            .back()
            .is_some_and(|&(last, _)| now.saturating_duration_since(last) > WINDOW)
        {
            // a new recording: start over, from the previous floor
            self.remembered = self.floor();
            self.history.clear();
        }
        self.history.push_back((now, reading));
        while self
            .history
            .front()
            .is_some_and(|&(t, _)| now.saturating_duration_since(t) > WINDOW)
        {
            self.history.pop_front();
        }
        let floor = self.floor();
        if self.history.len() > 1 {
            self.remembered = floor;
        }
        let span = (LOUD_DB - floor - MARGIN_DB).clamp(MIN_SPAN_DB, MAX_SPAN_DB);
        ((reading - floor - MARGIN_DB) / span).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    /// Feed `db` every 80 ms for `ms`; returns the last level.
    fn feed(meter: &mut Meter, now: &mut Instant, ms: u64, db: f32) -> f32 {
        let mut level = 0.0;
        for _ in 0..ms / 80 {
            *now += Duration::from_millis(80);
            level = meter.level(rms(db), *now);
        }
        level
    }

    #[test]
    fn quiet_microphone() {
        let (mut meter, mut now) = (Meter::new(), Instant::now());
        assert_eq!(feed(&mut meter, &mut now, 1500, -60.0), 0.0);
        let speech = feed(&mut meter, &mut now, 160, -24.0);
        assert!(speech > 0.85, "{speech}");
        let soft = feed(&mut meter, &mut now, 80, -40.0);
        assert!(soft > 0.3 && soft < 0.6, "{soft}");
    }

    #[test]
    fn hot_microphone_noise_shows_as_silence() {
        let (mut meter, mut now) = (Meter::new(), Instant::now());
        // a laptop mic boosted by PipeWire: -22 dBFS of noise
        feed(&mut meter, &mut now, 1500, -22.0);
        assert!((meter.floor() + 22.0).abs() < 0.01);
        assert_eq!(feed(&mut meter, &mut now, 400, -21.0), 0.0);
        // speech still moves the meter, and loud speech fills it
        let speech = feed(&mut meter, &mut now, 80, -10.0);
        assert!(speech > 0.3 && speech < 0.7, "{speech}");
        let loud = feed(&mut meter, &mut now, 80, -2.0);
        assert!(loud > 0.85, "{loud}");
    }

    #[test]
    fn speech_doesnt_become_the_floor() {
        let (mut meter, mut now) = (Meter::new(), Instant::now());
        feed(&mut meter, &mut now, 1500, -50.0);
        // two seconds of continuous speech (the floor holds: the window still has the quiet part) ...
        let speech = feed(&mut meter, &mut now, 2000, -20.0);
        assert!(speech > 0.7, "{speech}");
        assert!((meter.floor() + 50.0).abs() < 0.01);
    }

    #[test]
    fn a_new_recording_starts_from_the_previous_floor() {
        let (mut meter, mut now) = (Meter::new(), Instant::now());
        feed(&mut meter, &mut now, 2000, -30.0);
        // later, a recording that starts right away with speech
        now += Duration::from_secs(60);
        let first = meter.level(rms(-12.0), now);
        assert!(first > 0.5, "{first}");
        assert!((meter.floor() + 30.0).abs() < 0.01);
    }

    #[test]
    fn floor_is_bounded() {
        let (mut meter, mut now) = (Meter::new(), Instant::now());
        feed(&mut meter, &mut now, 2000, -120.0);
        assert_eq!(meter.floor(), LOWEST_FLOOR_DB);
        feed(&mut meter, &mut now, 3000, -1.0);
        assert_eq!(meter.floor(), HIGHEST_FLOOR_DB);
    }
}
