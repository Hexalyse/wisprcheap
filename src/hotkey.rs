//! Push-to-talk state machine driven by global key events.
//!
//! Hold the dictation combo to record, release to stop; double-tap it for hands-free.
//! Holding the command combo (which usually extends the dictation one, e.g. Ctrl+Win+Alt) records a command instead.
//! The add-word combo fires once when pressed; a recording started by the shared keys is discarded.
//!
//! Never inject keys while the hotkey is held: any key combined with Ctrl+Win can trigger a Windows
//! shortcut (e.g. Ctrl+Win+F24 toggles the touchpad).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::config::HotkeyConfig;

/// Key codes, identical to libuiohook's (so names match the TypeScript version's `UiohookKey`).
pub type KeyCode = u32;

pub mod keys {
    use super::KeyCode;
    pub const CTRL: KeyCode = 0x001D;
    pub const CTRL_RIGHT: KeyCode = 0x0E1D;
    pub const ALT: KeyCode = 0x0038;
    pub const ALT_RIGHT: KeyCode = 0x0E38;
    pub const SHIFT: KeyCode = 0x002A;
    pub const SHIFT_RIGHT: KeyCode = 0x0036;
    pub const META: KeyCode = 0x0E5B;
    pub const META_RIGHT: KeyCode = 0x0E5C;
    /// Keys without a name get codes above this (they never match a combo).
    pub const UNKNOWN_BASE: KeyCode = 0x10000;
}

pub const KEY_NAMES: &[(&str, KeyCode)] = &[
    ("Backspace", 0x000E),
    ("Tab", 0x000F),
    ("Enter", 0x001C),
    ("CapsLock", 0x003A),
    ("Escape", 0x0001),
    ("Space", 0x0039),
    ("PageUp", 0x0E49),
    ("PageDown", 0x0E51),
    ("End", 0x0E4F),
    ("Home", 0x0E47),
    ("ArrowLeft", 0xE04B),
    ("ArrowUp", 0xE048),
    ("ArrowRight", 0xE04D),
    ("ArrowDown", 0xE050),
    ("Insert", 0x0E52),
    ("Delete", 0x0E53),
    ("0", 0x000B),
    ("1", 0x0002),
    ("2", 0x0003),
    ("3", 0x0004),
    ("4", 0x0005),
    ("5", 0x0006),
    ("6", 0x0007),
    ("7", 0x0008),
    ("8", 0x0009),
    ("9", 0x000A),
    ("A", 0x001E),
    ("B", 0x0030),
    ("C", 0x002E),
    ("D", 0x0020),
    ("E", 0x0012),
    ("F", 0x0021),
    ("G", 0x0022),
    ("H", 0x0023),
    ("I", 0x0017),
    ("J", 0x0024),
    ("K", 0x0025),
    ("L", 0x0026),
    ("M", 0x0032),
    ("N", 0x0031),
    ("O", 0x0018),
    ("P", 0x0019),
    ("Q", 0x0010),
    ("R", 0x0013),
    ("S", 0x001F),
    ("T", 0x0014),
    ("U", 0x0016),
    ("V", 0x002F),
    ("W", 0x0011),
    ("X", 0x002D),
    ("Y", 0x0015),
    ("Z", 0x002C),
    ("Numpad0", 0x0052),
    ("Numpad1", 0x004F),
    ("Numpad2", 0x0050),
    ("Numpad3", 0x0051),
    ("Numpad4", 0x004B),
    ("Numpad5", 0x004C),
    ("Numpad6", 0x004D),
    ("Numpad7", 0x0047),
    ("Numpad8", 0x0048),
    ("Numpad9", 0x0049),
    ("NumpadMultiply", 0x0037),
    ("NumpadAdd", 0x004E),
    ("NumpadSubtract", 0x004A),
    ("NumpadDecimal", 0x0053),
    ("NumpadDivide", 0x0E35),
    ("NumpadEnter", 0x0E1C),
    ("NumpadEnd", 0xEE4F),
    ("NumpadArrowDown", 0xEE50),
    ("NumpadPageDown", 0xEE51),
    ("NumpadArrowLeft", 0xEE4B),
    ("NumpadArrowRight", 0xEE4D),
    ("NumpadHome", 0xEE47),
    ("NumpadArrowUp", 0xEE48),
    ("NumpadPageUp", 0xEE49),
    ("NumpadInsert", 0xEE52),
    ("NumpadDelete", 0xEE53),
    ("F1", 0x003B),
    ("F2", 0x003C),
    ("F3", 0x003D),
    ("F4", 0x003E),
    ("F5", 0x003F),
    ("F6", 0x0040),
    ("F7", 0x0041),
    ("F8", 0x0042),
    ("F9", 0x0043),
    ("F10", 0x0044),
    ("F11", 0x0057),
    ("F12", 0x0058),
    ("F13", 0x005B),
    ("F14", 0x005C),
    ("F15", 0x005D),
    ("F16", 0x0063),
    ("F17", 0x0064),
    ("F18", 0x0065),
    ("F19", 0x0066),
    ("F20", 0x0067),
    ("F21", 0x0068),
    ("F22", 0x0069),
    ("F23", 0x006A),
    ("F24", 0x006B),
    ("Semicolon", 0x0027),
    ("Equal", 0x000D),
    ("Comma", 0x0033),
    ("Minus", 0x000C),
    ("Period", 0x0034),
    ("Slash", 0x0035),
    ("Backquote", 0x0029),
    ("BracketLeft", 0x001A),
    ("Backslash", 0x002B),
    ("BracketRight", 0x001B),
    ("Quote", 0x0028),
    ("Ctrl", keys::CTRL),
    ("CtrlRight", keys::CTRL_RIGHT),
    ("Alt", keys::ALT),
    ("AltRight", keys::ALT_RIGHT),
    ("Shift", keys::SHIFT),
    ("ShiftRight", keys::SHIFT_RIGHT),
    ("Meta", keys::META),
    ("MetaRight", keys::META_RIGHT),
    ("NumLock", 0x0045),
    ("ScrollLock", 0x0046),
    ("PrintScreen", 0x0E37),
];

/// Code of a key name ("F13", "A", "Space"...), case-insensitive.
pub fn key_code(name: &str) -> Option<KeyCode> {
    KEY_NAMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|&(_, c)| c)
}

fn resolve_key(name: &str) -> Result<Vec<KeyCode>> {
    use keys::*;
    let codes = match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => vec![CTRL, CTRL_RIGHT],
        "ctrlleft" => vec![CTRL],
        "ctrlright" => vec![CTRL_RIGHT],
        "shift" => vec![SHIFT, SHIFT_RIGHT],
        "shiftleft" => vec![SHIFT],
        "shiftright" => vec![SHIFT_RIGHT],
        "alt" => vec![ALT, ALT_RIGHT],
        "altleft" => vec![ALT],
        "altright" => vec![ALT_RIGHT],
        "win" | "meta" | "super" => vec![META, META_RIGHT],
        "winleft" | "metaleft" => vec![META],
        "winright" | "metaright" => vec![META_RIGHT],
        _ => match key_code(name) {
            Some(code) => vec![code],
            None => bail!(
                "Unknown hotkey key \"{name}\". Use e.g. Ctrl, Shift, Alt, Win, CtrlRight, F13, Space."
            ),
        },
    };
    Ok(codes)
}

/// Each entry is one key of the combo, as the set of codes that satisfy it (e.g. left or right Ctrl).
type Combo = Vec<HashSet<KeyCode>>;

fn to_combo(keys: &[String]) -> Result<Option<Combo>> {
    if keys.is_empty() {
        return Ok(None);
    }
    let combo = keys
        .iter()
        .map(|k| resolve_key(k).map(|codes| codes.into_iter().collect()))
        .collect::<Result<Combo>>()?;
    Ok(Some(combo))
}

/// Check the key names of a hotkey config (used when loading the config).
pub fn validate(opts: &HotkeyConfig) -> Result<()> {
    to_combo(&opts.keys)?;
    to_combo(&opts.command_keys)?;
    to_combo(&opts.add_word_keys)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dictation,
    Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    Tap,
    OtherKey,
    Forced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Start recording.
    Start(Mode),
    /// The recording switched mode while held (e.g. Alt pressed during Ctrl+Win: dictation -> command).
    Mode(Mode),
    /// Stop recording and process it.
    Stop(Mode),
    /// The recording switched to hands-free (double-tap).
    Lock,
    /// Discard the recording.
    Cancel(CancelReason),
    /// The add-word shortcut was pressed.
    AddWord,
}

/// What a key event produced: events for the app, and possibly a tap timer to arm.
#[derive(Debug, Default)]
pub struct Output {
    pub events: Vec<HotkeyEvent>,
    /// (generation, delay): call `tap_timeout(generation)` after the delay.
    pub arm_tap_timer: Option<(u64, Duration)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Holding,
    TapPending,
    HandsFree,
    Suppressed,
}

pub struct PushToTalk {
    opts: HotkeyConfig,
    dictation: Combo,
    command: Option<Combo>,
    add_word: Option<Combo>,
    all_codes: HashSet<KeyCode>,
    pressed: HashSet<KeyCode>,
    state: State,
    mode: Mode,
    pressed_at: Instant,
    tap_generation: u64,
    ignore_other_keys_until: Instant,
    enabled: bool,
}

impl PushToTalk {
    pub fn new(opts: &HotkeyConfig) -> Result<Self> {
        let mut ptt = Self {
            opts: opts.clone(),
            dictation: Vec::new(),
            command: None,
            add_word: None,
            all_codes: HashSet::new(),
            pressed: HashSet::new(),
            state: State::Idle,
            mode: Mode::Dictation,
            pressed_at: Instant::now(),
            tap_generation: 0,
            ignore_other_keys_until: Instant::now(),
            enabled: true,
        };
        ptt.configure(opts)?;
        Ok(ptt)
    }

    /// Apply new hotkey settings (config reload). Resets any hold in progress.
    pub fn configure(&mut self, opts: &HotkeyConfig) -> Result<()> {
        let dictation = to_combo(&opts.keys)?.unwrap_or_default();
        let command = to_combo(&opts.command_keys)?;
        let add_word = to_combo(&opts.add_word_keys)?;
        self.all_codes = [Some(&dictation), command.as_ref(), add_word.as_ref()]
            .into_iter()
            .flatten()
            .flat_map(|combo| combo.iter().flat_map(|g| g.iter().copied()))
            .collect();
        self.opts = opts.clone();
        self.dictation = dictation;
        self.command = command;
        self.add_word = add_word;
        self.reset();
        Ok(())
    }

    pub fn label(&self) -> String {
        self.opts.keys.join(" + ")
    }

    pub fn command_label(&self) -> Option<String> {
        self.command
            .as_ref()
            .map(|_| self.opts.command_keys.join(" + "))
    }

    pub fn add_word_label(&self) -> Option<String> {
        self.add_word
            .as_ref()
            .map(|_| self.opts.add_word_keys.join(" + "))
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// True while any key used by one of the shortcuts is physically held.
    pub fn is_any_hotkey_key_down(&self) -> bool {
        self.pressed.iter().any(|c| self.all_codes.contains(c))
    }

    pub fn is_win_down(&self) -> bool {
        self.pressed.contains(&keys::META) || self.pressed.contains(&keys::META_RIGHT)
    }

    /// Call right before injecting keystrokes so they aren't mistaken for user input.
    pub fn mark_injection(&mut self, ms: u64) {
        self.ignore_other_keys_until = Instant::now() + Duration::from_millis(ms);
    }

    /// Go back to idle without emitting anything (e.g. after hitting the max duration).
    pub fn reset(&mut self) {
        self.tap_generation += 1; // cancels a pending tap timer
        self.state = State::Idle;
    }

    /// While disabled, key presses are still tracked but never start a recording.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.reset();
        }
    }

    fn held(&self, combo: Option<&Combo>) -> bool {
        combo.is_some_and(|c| {
            c.iter()
                .all(|group| group.iter().any(|code| self.pressed.contains(code)))
        })
    }

    fn session_held(&self) -> bool {
        self.held(Some(&self.dictation)) || self.held(self.command.as_ref())
    }

    pub fn key_down(&mut self, code: KeyCode) -> Output {
        self.key_down_at(code, Instant::now())
    }

    pub fn key_up(&mut self, code: KeyCode) -> Output {
        self.key_up_at(code, Instant::now())
    }

    pub fn key_down_at(&mut self, code: KeyCode, now: Instant) -> Output {
        let mut out = Output::default();
        if self.pressed.contains(&code) {
            return out; // auto-repeat
        }
        let was_session = self.session_held();
        let was_add_word = self.held(self.add_word.as_ref());
        self.pressed.insert(code);

        if !self.all_codes.contains(&code) {
            let injected = now < self.ignore_other_keys_until;
            if self.opts.cancel_on_other_key
                && !injected
                && matches!(self.state, State::Holding | State::TapPending)
            {
                self.tap_generation += 1;
                self.state = State::Idle;
                out.events.push(HotkeyEvent::Cancel(CancelReason::OtherKey));
            }
            return out;
        }
        if !self.enabled || self.state == State::Suppressed {
            return out;
        }

        if !was_add_word && self.held(self.add_word.as_ref()) {
            if matches!(self.state, State::Holding | State::TapPending) {
                self.tap_generation += 1;
                out.events.push(HotkeyEvent::Cancel(CancelReason::Forced));
            }
            self.state = State::Suppressed; // ignore everything until all shortcut keys are released
            out.events.push(HotkeyEvent::AddWord);
            return out;
        }

        if !was_session && self.session_held() {
            self.on_combo_down(now, &mut out);
        } else if self.state == State::Holding
            && self.mode == Mode::Dictation
            && self.held(self.command.as_ref())
        {
            self.mode = Mode::Command;
            out.events.push(HotkeyEvent::Mode(Mode::Command));
        }
        out
    }

    pub fn key_up_at(&mut self, code: KeyCode, now: Instant) -> Output {
        let mut out = Output::default();
        let was_session = self.session_held();
        self.pressed.remove(&code);
        if self.state == State::Suppressed {
            if !self.is_any_hotkey_key_down() {
                self.state = State::Idle;
            }
            return out;
        }
        if was_session && !self.session_held() {
            self.on_combo_up(now, &mut out);
        }
        out
    }

    /// The double-tap window elapsed (only acts if `generation` is still the current timer).
    pub fn tap_timeout(&mut self, generation: u64) -> Vec<HotkeyEvent> {
        if generation != self.tap_generation || self.state != State::TapPending {
            return Vec::new();
        }
        self.state = State::Idle;
        vec![HotkeyEvent::Cancel(CancelReason::Tap)]
    }

    fn on_combo_down(&mut self, now: Instant, out: &mut Output) {
        match self.state {
            State::Idle => {
                self.state = State::Holding;
                self.pressed_at = now;
                self.mode = if self.held(self.command.as_ref()) {
                    Mode::Command
                } else {
                    Mode::Dictation
                };
                out.events.push(HotkeyEvent::Start(self.mode));
            }
            State::TapPending => {
                self.tap_generation += 1;
                self.state = State::HandsFree;
                out.events.push(HotkeyEvent::Lock);
            }
            State::HandsFree => {
                self.state = State::Idle;
                out.events.push(HotkeyEvent::Stop(Mode::Dictation));
            }
            _ => {}
        }
    }

    fn on_combo_up(&mut self, now: Instant, out: &mut Output) {
        if self.state != State::Holding {
            return;
        }
        let held = now.saturating_duration_since(self.pressed_at);
        if self.mode == Mode::Dictation
            && self.opts.hands_free_double_tap
            && held < Duration::from_millis(self.opts.tap_max_ms)
        {
            // Might be the first half of a double-tap: keep recording and wait for the second press.
            self.state = State::TapPending;
            self.tap_generation += 1;
            out.arm_tap_timer = Some((
                self.tap_generation,
                Duration::from_millis(self.opts.double_tap_window_ms),
            ));
            return;
        }
        self.state = State::Idle;
        out.events.push(HotkeyEvent::Stop(self.mode));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F13: KeyCode = 0x5B;
    const F14: KeyCode = 0x5C;
    const F15: KeyCode = 0x5D;
    const F16: KeyCode = 0x63;
    const F17: KeyCode = 0x64;

    struct Harness {
        ptt: PushToTalk,
        now: Instant,
        events: Vec<String>,
        timer: Option<(u64, Instant)>,
    }

    impl Harness {
        fn new() -> Self {
            let opts = HotkeyConfig {
                keys: vec!["F13".into(), "F14".into()],
                command_keys: vec!["F13".into(), "F14".into(), "F16".into()],
                add_word_keys: vec!["F13".into(), "F14".into(), "F17".into()],
                hands_free_double_tap: true,
                tap_max_ms: 250,
                double_tap_window_ms: 150,
                cancel_on_other_key: true,
            };
            Self {
                ptt: PushToTalk::new(&opts).unwrap(),
                now: Instant::now(),
                events: Vec::new(),
                timer: None,
            }
        }

        fn record(&mut self, out: Output) {
            self.push(out.events);
            if let Some((generation, delay)) = out.arm_tap_timer {
                self.timer = Some((generation, self.now + delay));
            }
        }

        fn push(&mut self, events: Vec<HotkeyEvent>) {
            for e in events {
                self.events.push(match e {
                    HotkeyEvent::Start(m) => format!("start:{}", mode(m)),
                    HotkeyEvent::Mode(m) => format!("mode:{}", mode(m)),
                    HotkeyEvent::Stop(m) => format!("stop:{}", mode(m)),
                    HotkeyEvent::Lock => "lock".into(),
                    HotkeyEvent::Cancel(r) => format!(
                        "cancel:{}",
                        match r {
                            CancelReason::Tap => "tap",
                            CancelReason::OtherKey => "other-key",
                            CancelReason::Forced => "forced",
                        }
                    ),
                    HotkeyEvent::AddWord => "add-word".into(),
                });
            }
        }

        fn down(&mut self, codes: &[KeyCode]) {
            for &c in codes {
                let out = self.ptt.key_down_at(c, self.now);
                self.record(out);
            }
        }

        fn up(&mut self, codes: &[KeyCode]) {
            for &c in codes {
                let out = self.ptt.key_up_at(c, self.now);
                self.record(out);
            }
        }

        fn sleep(&mut self, ms: u64) {
            self.now += Duration::from_millis(ms);
            if let Some((generation, at)) = self.timer
                && at <= self.now
            {
                self.timer = None;
                let events = self.ptt.tap_timeout(generation);
                self.push(events);
            }
        }
    }

    fn mode(m: Mode) -> &'static str {
        match m {
            Mode::Dictation => "dictation",
            Mode::Command => "command",
        }
    }

    #[test]
    fn hold_and_release_dictates() {
        let mut h = Harness::new();
        h.down(&[F13, F14]);
        h.sleep(300);
        h.up(&[F14, F13]);
        assert_eq!(h.events, ["start:dictation", "stop:dictation"]);
    }

    #[test]
    fn single_short_tap_is_cancelled() {
        let mut h = Harness::new();
        h.down(&[F13, F14]);
        h.up(&[F14, F13]);
        h.sleep(250);
        assert_eq!(h.events, ["start:dictation", "cancel:tap"]);
    }

    #[test]
    fn double_tap_locks_hands_free() {
        let mut h = Harness::new();
        h.down(&[F13, F14]);
        h.up(&[F14, F13]);
        h.sleep(50);
        h.down(&[F13, F14]);
        h.up(&[F14, F13]);
        h.sleep(300);
        h.down(&[F13, F14]);
        h.up(&[F14, F13]);
        assert_eq!(h.events, ["start:dictation", "lock", "stop:dictation"]);
    }

    #[test]
    fn other_key_cancels() {
        let mut h = Harness::new();
        h.down(&[F13, F14, F15]);
        h.up(&[F15, F14, F13]);
        assert_eq!(h.events, ["start:dictation", "cancel:other-key"]);
    }

    #[test]
    fn partial_combo_does_nothing() {
        let mut h = Harness::new();
        h.down(&[F13]);
        h.up(&[F13]);
        assert!(h.events.is_empty());
    }

    #[test]
    fn auto_repeat_ignored() {
        let mut h = Harness::new();
        h.down(&[F13, F14, F14, F14]);
        h.sleep(300);
        h.up(&[F14, F13]);
        assert_eq!(h.events, ["start:dictation", "stop:dictation"]);
    }

    #[test]
    fn command_combo_directly() {
        let mut h = Harness::new();
        h.down(&[F16, F13, F14]);
        h.up(&[F14, F13, F16]);
        assert_eq!(h.events, ["start:command", "stop:command"]);
    }

    #[test]
    fn extra_key_switches_to_command() {
        let mut h = Harness::new();
        h.down(&[F13, F14]);
        h.sleep(50);
        h.down(&[F16]);
        h.up(&[F16]);
        h.sleep(300);
        h.up(&[F14, F13]);
        assert_eq!(
            h.events,
            ["start:dictation", "mode:command", "stop:command"]
        );
    }

    #[test]
    fn add_word_cancels_shared_recording() {
        let mut h = Harness::new();
        h.down(&[F13, F14, F17]);
        h.up(&[F17, F14, F13]);
        assert_eq!(h.events, ["start:dictation", "cancel:forced", "add-word"]);
    }

    #[test]
    fn add_word_first_then_dictation_works() {
        let mut h = Harness::new();
        h.down(&[F17, F13, F14]);
        h.up(&[F14, F13, F17]);
        h.down(&[F13, F14]);
        h.sleep(300);
        h.up(&[F14, F13]);
        assert_eq!(h.events, ["add-word", "start:dictation", "stop:dictation"]);
    }

    #[test]
    fn disabled_does_nothing() {
        let mut h = Harness::new();
        h.ptt.set_enabled(false);
        h.down(&[F13, F14]);
        h.up(&[F14, F13]);
        h.down(&[F17, F13, F14]);
        h.up(&[F14, F13, F17]);
        assert!(h.events.is_empty());
    }

    #[test]
    fn configure_applies_new_keys() {
        let mut h = Harness::new();
        h.ptt
            .configure(&HotkeyConfig {
                keys: vec!["F15".into()],
                command_keys: vec![],
                add_word_keys: vec![],
                hands_free_double_tap: false,
                tap_max_ms: 250,
                double_tap_window_ms: 150,
                cancel_on_other_key: true,
            })
            .unwrap();
        h.down(&[F13, F14]);
        h.up(&[F14, F13]);
        h.down(&[F15]);
        h.sleep(50);
        h.up(&[F15]);
        assert_eq!(h.events, ["start:dictation", "stop:dictation"]);
        assert_eq!(h.ptt.command_label(), None);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = resolve_key("Hyper").unwrap_err().to_string();
        assert!(err.contains("Unknown hotkey key"));
    }
}
