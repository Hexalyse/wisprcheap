//! Delivering text: clipboard + simulated Ctrl+V, and capturing the selection with Ctrl+C.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
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

/// Ctrl+`letter` into the focused app, or Ctrl+Shift+`letter` when it's a terminal.
async fn inject(hotkey: &SharedHotkey, letter: Letter, terminal_apps: &[String]) -> Result<()> {
    hotkey.lock().unwrap().mark_injection(200);
    let extra = terminal_apps.to_vec();
    tokio::task::spawn_blocking(move || {
        let terminal = crate::focus::terminal_focused(&extra);
        tap_ctrl(letter, terminal)
    })
    .await
    .map_err(|e| anyhow!("key injection task failed: {e}"))?
}

fn hotkey_released(hotkey: &SharedHotkey) -> impl Fn() -> bool + '_ {
    move || !hotkey.lock().unwrap().is_any_hotkey_key_down()
}

/// Copy the text to the clipboard and paste it into the focused app with a simulated Ctrl+V
/// (Ctrl+Shift+V in a terminal).
pub async fn deliver(text: &str, opts: &OutputConfig, hotkey: &SharedHotkey) -> Result<Delivered> {
    deliver_with(
        text,
        opts,
        hotkey,
        injection_disabled(),
        clipboard::read,
        clipboard::write,
        || inject(hotkey, Letter::V, &opts.terminal_apps),
        Duration::from_millis(30),
        Duration::from_millis(300),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn deliver_with<ReadF, ReadFut, WriteF, WriteFut, InjF, InjFut>(
    text: &str,
    opts: &OutputConfig,
    hotkey: &SharedHotkey,
    no_inject: bool,
    mut read_cb: ReadF,
    mut write_cb: WriteF,
    inject_paste: InjF,
    settle_delay: Duration,
    restore_delay: Duration,
) -> Result<Delivered>
where
    ReadF: FnMut() -> ReadFut,
    ReadFut: Future<Output = Result<String>>,
    WriteF: FnMut(String) -> WriteFut,
    WriteFut: Future<Output = Result<()>>,
    InjF: FnOnce() -> InjFut,
    InjFut: Future<Output = Result<()>>,
{
    let previous = if opts.paste && opts.restore_clipboard {
        read_cb().await.ok()
    } else {
        None
    };
    write_cb(text.to_string()).await?;
    if !opts.paste || no_inject {
        return Ok(Delivered::Clipboard);
    }

    // Pasting while the hotkey is still held would send e.g. Win+V (clipboard history) instead of Ctrl+V.
    let released = wait_until(hotkey_released(hotkey), Duration::from_secs(5)).await;
    if !released && hotkey.lock().unwrap().is_win_down() {
        return Ok(Delivered::Clipboard);
    }

    if !settle_delay.is_zero() {
        sleep(settle_delay).await; // let the clipboard settle
    }
    if let Err(e) = inject_paste().await {
        crate::warn!("[output] could not paste ({e}); text kept on the clipboard");
        return Ok(Delivered::Clipboard);
    }

    if let Some(previous) = previous {
        if !restore_delay.is_zero() {
            sleep(restore_delay).await; // the target app reads the clipboard asynchronously
        }
        let _ = write_cb(previous).await;
    }
    Ok(Delivered::Pasted)
}

pub struct Selection {
    /// The selected text, or None when nothing was selected (or the app didn't copy).
    pub text: Option<String>,
    /// Clipboard content before the capture, to put back if nothing gets pasted.
    pub previous: Option<String>,
}

/// Copy the focused app's selection with a simulated Ctrl+C (Ctrl+Shift+C in a terminal), once the hotkey
/// is released.
/// A marker is put on the clipboard first: if it's still there afterwards, nothing was selected.
pub async fn capture_selection(
    hotkey: &SharedHotkey,
    terminal_apps: &[String],
) -> Result<Selection> {
    capture_selection_with(
        hotkey,
        injection_disabled(),
        clipboard::read,
        clipboard::write,
        || inject(hotkey, Letter::C, terminal_apps),
        Duration::from_millis(30),
        Duration::from_millis(60),
        Duration::from_millis(700),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn capture_selection_with<ReadF, ReadFut, WriteF, WriteFut, InjF, InjFut>(
    hotkey: &SharedHotkey,
    no_inject: bool,
    mut read_cb: ReadF,
    mut write_cb: WriteF,
    inject_copy: InjF,
    settle_delay: Duration,
    poll_interval: Duration,
    poll_timeout: Duration,
) -> Result<Selection>
where
    ReadF: FnMut() -> ReadFut,
    ReadFut: Future<Output = Result<String>>,
    WriteF: FnMut(String) -> WriteFut,
    WriteFut: Future<Output = Result<()>>,
    InjF: FnOnce() -> InjFut,
    InjFut: Future<Output = Result<()>>,
{
    let previous = read_cb().await.ok();
    if no_inject {
        return Ok(Selection {
            text: None,
            previous,
        });
    }
    if !wait_until(hotkey_released(hotkey), Duration::from_secs(5)).await {
        return Ok(Selection {
            text: None,
            previous,
        });
    }

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let marker = format!("\u{2063}wisprcheap-selection-{millis}");
    write_cb(marker.clone())
        .await
        .map_err(|e| anyhow!("could not access the clipboard to capture the selection: {e}"))?;
    if !settle_delay.is_zero() {
        sleep(settle_delay).await;
    }
    if let Err(e) = inject_copy().await {
        if let Some(p) = previous.as_deref() {
            let _ = write_cb(p.to_string()).await;
        }
        return Err(anyhow!(
            "could not send Copy to capture the selection: {e}"
        ));
    }

    let deadline = Instant::now() + poll_timeout;
    while Instant::now() < deadline {
        if !poll_interval.is_zero() {
            sleep(poll_interval).await;
        }
        let current = read_cb().await.unwrap_or_else(|_| marker.clone());
        if current != marker {
            let text = (!current.is_empty()).then_some(current);
            return Ok(Selection { text, previous });
        }
    }
    if let Some(p) = previous.as_deref() {
        let _ = write_cb(p.to_string()).await;
    }
    Ok(Selection {
        text: None,
        previous,
    })
}

pub async fn restore_clipboard(previous: Option<&str>) {
    if let Some(p) = previous {
        let _ = clipboard::write(p).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotkeyConfig;

    fn idle_hotkey() -> SharedHotkey {
        Arc::new(Mutex::new(PushToTalk::new(&HotkeyConfig::default()).unwrap()))
    }

    #[tokio::test]
    async fn capture_selection_aborts_when_marker_write_fails() {
        let hk = idle_hotkey();
        let injected = Arc::new(Mutex::new(false));
        let injected_flag = injected.clone();
        let result = capture_selection_with(
            &hk,
            false,
            || async { Ok("secret-password-on-clipboard".to_string()) },
            |_| async { Err(anyhow!("clipboard busy")) },
            move || {
                *injected_flag.lock().unwrap() = true;
                async { Ok(()) }
            },
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(50),
        )
        .await;
        assert!(result.is_err());
        assert!(!*injected.lock().unwrap());
    }

    #[tokio::test]
    async fn deliver_keeps_transcript_and_reports_clipboard_on_injection_failure() {
        let hk = idle_hotkey();
        let clip = Arc::new(Mutex::new("old-clipboard".to_string()));
        let opts = OutputConfig {
            paste: true,
            restore_clipboard: true,
            ..Default::default()
        };
        let r_clip = clip.clone();
        let w_clip = clip.clone();
        let delivered = deliver_with(
            "new transcript",
            &opts,
            &hk,
            false,
            move || {
                let val = r_clip.lock().unwrap().clone();
                async move { Ok(val) }
            },
            move |s| {
                *w_clip.lock().unwrap() = s;
                async { Ok(()) }
            },
            || async { Err(anyhow!("SendInput blocked")) },
            Duration::ZERO,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(delivered, Delivered::Clipboard);
        assert_eq!(*clip.lock().unwrap(), "new transcript");
    }
}
