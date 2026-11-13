//! Hyprland IPC: the active window, so the edge glow can sit on it and pasting can tell terminals apart.
//! Requests go to Hyprland's own socket (no `hyprctl` process): about a millisecond each.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::Deserialize;

/// The active window, in logical pixels relative to its monitor's top-left corner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWindow {
    /// Output name (`eDP-1`, `DP-2`...), as GDK's monitor connector.
    pub monitor: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// What kind of window has the focus.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Focused {
    /// Window class (the Wayland app id).
    pub class: String,
    /// Hyprland tags, without the trailing `*` of dynamic ones (Omarchy tags terminals `terminal`).
    pub tags: Vec<String>,
}

#[derive(Deserialize)]
struct Client {
    at: [i32; 2],
    size: [i32; 2],
    monitor: i64,
    #[serde(default)]
    class: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "yes")]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
}

#[derive(Deserialize)]
struct Monitor {
    id: i64,
    name: String,
    x: i32,
    y: i32,
}

fn yes() -> bool {
    true
}

/// Running under Hyprland: its IPC socket.
fn socket() -> Option<std::path::PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(
        std::path::PathBuf::from(runtime)
            .join("hypr")
            .join(signature)
            .join(".socket.sock"),
    )
}

fn request(command: &str) -> Option<String> {
    let mut stream = UnixStream::connect(socket()?).ok()?;
    let timeout = Some(Duration::from_millis(150));
    stream.set_read_timeout(timeout).ok()?;
    stream.set_write_timeout(timeout).ok()?;
    stream.write_all(command.as_bytes()).ok()?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).ok()?;
    Some(reply)
}

/// The focused window, or None (no window focused, not Hyprland, or no answer).
pub fn active_window() -> Option<ActiveWindow> {
    let active = request("j/activewindow")?;
    let monitors = request("j/monitors")?;
    parse(&active, &monitors)
}

/// Enable or disable the Escape bind of the Omarchy integration (`wisprcheap_escape`, defined in the
/// `shortcuts.lua` that Omarchy loads while wisprcheap is its dictation backend): enabled, Escape cancels
/// the recording without reaching the focused app. Does nothing without Hyprland or that bind.
pub fn set_escape_bind(enabled: bool) {
    if socket().is_some() {
        let _ = request(&format!(
            "eval if wisprcheap_escape then wisprcheap_escape:set_enabled({enabled}) end"
        ));
    }
}

/// The focused window's class and tags, or None (no window focused, not Hyprland, or no answer).
pub fn focused() -> Option<Focused> {
    parse_focused(&request("j/activewindow")?)
}

fn parse_focused(active: &str) -> Option<Focused> {
    let client: Client = serde_json::from_str(active).ok()?;
    Some(Focused {
        class: client.class,
        tags: client
            .tags
            .into_iter()
            .map(|t| t.trim_end_matches('*').to_string())
            .collect(),
    })
}

fn parse(active: &str, monitors: &str) -> Option<ActiveWindow> {
    // `{}` when no window has the focus
    let client: Client = serde_json::from_str(active).ok()?;
    if !client.mapped || client.hidden || client.size[0] <= 0 || client.size[1] <= 0 {
        return None;
    }
    let monitors: Vec<Monitor> = serde_json::from_str(monitors).ok()?;
    let monitor = monitors.into_iter().find(|m| m.id == client.monitor)?;
    Some(ActiveWindow {
        x: client.at[0] - monitor.x,
        y: client.at[1] - monitor.y,
        width: client.size[0],
        height: client.size[1],
        monitor: monitor.name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str = r#"[
        {"id": 0, "name": "eDP-1", "x": 0, "y": 0, "width": 3840, "height": 2400, "scale": 2.0},
        {"id": 1, "name": "DP-2", "x": 1920, "y": -200, "width": 2560, "height": 1440, "scale": 1.0}
    ]"#;

    #[test]
    fn window_is_relative_to_its_monitor() {
        let active = r#"{"address": "0x1", "mapped": true, "hidden": false, "at": [2000, -150],
            "size": [800, 600], "monitor": 1, "fullscreen": 0}"#;
        assert_eq!(
            parse(active, MONITORS),
            Some(ActiveWindow {
                monitor: "DP-2".into(),
                x: 80,
                y: 50,
                width: 800,
                height: 600,
            })
        );
    }

    #[test]
    fn focused_class_and_tags() {
        let active = r#"{"at": [0, 0], "size": [800, 600], "monitor": 0, "class": "foot",
            "tags": ["terminal*", "pinned"]}"#;
        assert_eq!(
            parse_focused(active),
            Some(Focused {
                class: "foot".into(),
                tags: vec!["terminal".into(), "pinned".into()],
            })
        );
        assert_eq!(parse_focused("{}"), None);
    }

    #[test]
    fn no_focused_window() {
        assert_eq!(parse("{}", MONITORS), None);
        assert_eq!(parse("", MONITORS), None);
        let hidden =
            r#"{"mapped": true, "hidden": true, "at": [0, 0], "size": [10, 10], "monitor": 0}"#;
        assert_eq!(parse(hidden, MONITORS), None);
        let unknown_monitor = r#"{"at": [0, 0], "size": [10, 10], "monitor": 7}"#;
        assert_eq!(parse(unknown_monitor, MONITORS), None);
    }
}
