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
}

/// Press and release Ctrl+`letter` in the focused application.
pub fn tap_ctrl(letter: Letter) {
    imp::tap_ctrl(letter);
}
