//! Recording overlay: a small pill at the bottom center of the screen that shows when wisprcheap is listening
//! (with a live waveform), when it's working on the audio, and how it went. It never takes the focus and
//! clicks go through it. The look follows the Android app's bubble.
//!
//! Drawn in software (`paint`), animated by `scene`, and shown by a layered window on Windows or a GTK popup
//! on Linux (X11 only: Wayland doesn't let an app place its windows).

mod paint;
mod scene;

#[cfg(windows)]
#[path = "windows.rs"]
mod window;
#[cfg(target_os = "linux")]
#[path = "gtk.rs"]
mod window;

pub use window::Overlay;

use crate::hotkey::Mode;

/// What the app is doing, as far as the overlay is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OverlayStatus {
    /// `overlay.enabled` in the config.
    pub enabled: bool,
    /// Set while the microphone is open.
    pub recording: Option<Recording>,
    /// Recordings are waiting to be, or being, transcribed and processed.
    pub busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recording {
    pub mode: Mode,
    /// Started with a double-tap: the hotkey isn't held.
    pub hands_free: bool,
}

/// How a recording ended, shown for a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feedback {
    Pasted,
    /// Only copied to the clipboard (`output.paste: false`, or a retry from the tray).
    Copied,
    /// Nothing was heard, or the transcript was empty.
    Discarded,
    Error,
}
