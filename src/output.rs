//! Delivering text: clipboard + simulated Ctrl+V, and capturing the selection with Ctrl+C.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::time::sleep;

use crate::clipboard;
use crate::config::OutputConfig;
use crate::history::Delivered;
use crate::hotkey::PushToTalk;
use crate::keyboard::{Letter, tap_ctrl};

pub type SharedHotkey = Arc<Mutex<PushToTalk>>;

/// Tests set WISPRCHEAP_NO_INJECT=1 so they never send Ctrl+C / Ctrl+V to whatever window has focus.
fn injection_disabled() -> bool {
    std::env::var("WISPRCHEAP_NO_INJECT").is_ok_and(|v| v == "1")
}

async fn wait_until(predicate: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !predicate() {
        if Instant::now() > deadline {
            return false;
        }
        sleep(Duration::from_millis(20)).await;
    }
    true
}

async fn inject(hotkey: &SharedHotkey, letter: Letter) {
    hotkey.lock().unwrap().mark_injection(200);
    let _ = tokio::task::spawn_blocking(move || tap_ctrl(letter)).await;
}

fn hotkey_released(hotkey: &SharedHotkey) -> impl Fn() -> bool + '_ {
    move || !hotkey.lock().unwrap().is_any_hotkey_key_down()
}

/// Copy the text to the clipboard and paste it into the focused app with a simulated Ctrl+V.
pub async fn deliver(text: &str, opts: &OutputConfig, hotkey: &SharedHotkey) -> Result<Delivered> {
    let previous = if opts.paste && opts.restore_clipboard {
        clipboard::read().await.ok()
    } else {
        None
    };
    clipboard::write(text).await?;
    if !opts.paste || injection_disabled() {
        return Ok(Delivered::Clipboard);
    }

    // Pasting while the hotkey is still held would send e.g. Win+V (clipboard history) instead of Ctrl+V.
    let released = wait_until(hotkey_released(hotkey), Duration::from_secs(5)).await;
    if !released && hotkey.lock().unwrap().is_win_down() {
        return Ok(Delivered::Clipboard);
    }

    sleep(Duration::from_millis(30)).await; // let the clipboard settle
    inject(hotkey, Letter::V).await;

    if let Some(previous) = previous {
        sleep(Duration::from_millis(300)).await; // the target app reads the clipboard asynchronously
        let _ = clipboard::write(previous).await;
    }
    Ok(Delivered::Pasted)
}

pub struct Selection {
    /// The selected text, or None when nothing was selected (or the app didn't copy).
    pub text: Option<String>,
    /// Clipboard content before the capture, to put back if nothing gets pasted.
    pub previous: Option<String>,
}

/// Copy the focused app's selection with a simulated Ctrl+C, once the hotkey is released.
/// A marker is put on the clipboard first: if it's still there afterwards, nothing was selected.
pub async fn capture_selection(hotkey: &SharedHotkey) -> Selection {
    let previous = clipboard::read().await.ok();
    if injection_disabled() {
        return Selection {
            text: None,
            previous,
        };
    }
    if !wait_until(hotkey_released(hotkey), Duration::from_secs(5)).await {
        return Selection {
            text: None,
            previous,
        };
    }

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let marker = format!("\u{2063}wisprcheap-selection-{millis}");
    let _ = clipboard::write(marker.clone()).await;
    sleep(Duration::from_millis(30)).await;
    inject(hotkey, Letter::C).await;

    let deadline = Instant::now() + Duration::from_millis(700);
    while Instant::now() < deadline {
        sleep(Duration::from_millis(60)).await;
        let current = clipboard::read().await.unwrap_or_else(|_| marker.clone());
        if current != marker {
            let text = (!current.is_empty()).then_some(current);
            return Selection { text, previous };
        }
    }
    restore_clipboard(previous.as_deref()).await;
    Selection {
        text: None,
        previous,
    }
}

pub async fn restore_clipboard(previous: Option<&str>) {
    if let Some(p) = previous {
        let _ = clipboard::write(p).await;
    }
}
