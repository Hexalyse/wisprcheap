//! Global keyboard observation (never blocks keys) and Ctrl+<key> injection.

use tokio::sync::mpsc::UnboundedSender;

use crate::hotkey::KeyCode;

#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub down: bool,
}

/// Letter keys we inject together with Ctrl.
#[derive(Debug, Clone, Copy)]
pub enum Letter {
    C,
    V,
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod linux_input;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
use linux as imp;

pub struct Hook(imp::Hook);

impl Hook {
    /// Start the global keyboard hook; key events are sent to `tx`.
    pub fn start(tx: UnboundedSender<KeyEvent>) -> anyhow::Result<Self> {
        imp::Hook::start(tx).map(Hook)
    }

    pub fn stop(&self) {
        self.0.stop();
    }

    /// A setup problem to report once: the hook works, but only partly
    /// (Linux on Wayland without access to the input devices).
    pub fn warning(&self) -> Option<&str> {
        self.0.warning()
    }
}

/// Press and release Ctrl+`letter` in the focused application.
pub fn tap_ctrl(letter: Letter) {
    imp::tap_ctrl(letter);
}

/// `wisprcheap wayland`: the keyboard access status and the one-time setup. Returns (report, all good).
pub fn setup_report() -> (String, bool) {
    #[cfg(target_os = "linux")]
    {
        linux::setup_report()
    }
    #[cfg(not(target_os = "linux"))]
    {
        (
            "Nothing to set up: this is only needed on Linux, for Wayland sessions.".to_string(),
            true,
        )
    }
}
