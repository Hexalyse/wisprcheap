//! Is the focused window a terminal? Terminals paste with Ctrl+Shift+V and copy with Ctrl+Shift+C:
//! Ctrl+V types a literal ^V there, and Ctrl+C interrupts the running program.
//!
//! Linux only. On Hyprland, the focused window's class and tags come from its IPC socket (Omarchy tags
//! every terminal `terminal`, the same check its own "universal" Super+C / Super+V make); in X11
//! sessions, from `_NET_ACTIVE_WINDOW`'s `WM_CLASS`. Elsewhere it's unknown, and treated as not a terminal.

/// Window classes (Wayland app ids, or X11 `WM_CLASS`) of common terminals, lowercase.
const TERMINALS: &[&str] = &[
    "alacritty",
    "kitty",
    "foot",
    "footclient",
    "org.codeberg.dnkl.foot",
    "org.codeberg.dnkl.footclient",
    "com.mitchellh.ghostty",
    "ghostty",
    "wezterm",
    "org.wezfurlong.wezterm",
    "konsole",
    "org.kde.konsole",
    "yakuake",
    "org.kde.yakuake",
    "gnome-terminal",
    "gnome-terminal-server",
    "org.gnome.terminal",
    "org.gnome.console",
    "kgx",
    "org.gnome.ptyxis",
    "ptyxis",
    "xfce4-terminal",
    "lxterminal",
    "mate-terminal",
    "qterminal",
    "terminator",
    "tilix",
    "com.gexperts.tilix",
    "guake",
    "sakura",
    "terminology",
    "termite",
    "xterm",
    "uxterm",
    "urxvt",
    "rxvt",
    "st",
    "st-256color",
    "rio",
    "contour",
    "io.elementary.terminal",
    "com.raggesilver.blackbox",
    "cool-retro-term",
    "tabby",
    "warp",
    "dev.warp.warp",
];

/// Whether a window with this class and these tags is a terminal. `extra`: `output.terminalApps`.
pub fn is_terminal(class: &str, tags: &[String], extra: &[String]) -> bool {
    let class = class.to_ascii_lowercase();
    tags.iter().any(|t| t.trim_end_matches('*') == "terminal")
        || TERMINALS.contains(&class.as_str())
        // Omarchy's own terminal windows and TUIs
        || class.starts_with("org.omarchy.")
        || class.starts_with("tui.")
        || extra.iter().any(|e| e.eq_ignore_ascii_case(&class))
}

/// Whether the focused window is a terminal (blocking: a quick IPC or X11 request).
#[cfg(target_os = "linux")]
pub fn terminal_focused(extra: &[String]) -> bool {
    if let Some(window) = crate::hyprland::focused() {
        return is_terminal(&window.class, &window.tags, extra);
    }
    if crate::keyboard::is_wayland() {
        // Another compositor: no standard way to know the focused window.
        return false;
    }
    x11_active_class().is_some_and(|class| is_terminal(&class, &[], extra))
}

#[cfg(not(target_os = "linux"))]
pub fn terminal_focused(_extra: &[String]) -> bool {
    false
}

/// X11: the instance and class of `WM_CLASS` on the active window (either may identify a terminal).
#[cfg(target_os = "linux")]
fn x11_active_class() -> Option<String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

    let (connection, screen) = x11rb::connect(None).ok()?;
    let root = connection.setup().roots[screen].root;
    let active = connection
        .intern_atom(false, b"_NET_ACTIVE_WINDOW")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let window = connection
        .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()?;
    let class = connection
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
        .ok()?
        .reply()
        .ok()?
        .value;
    // "instance\0Class\0": the class names the application (XTerm, URxvt...), the instance may too.
    let mut parts = class
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned());
    let instance = parts.next()?;
    let class = parts.next().unwrap_or_default();
    Some(if is_terminal(&class, &[], &[]) {
        class
    } else {
        instance
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_terminals() {
        for class in [
            "foot",
            "Alacritty",
            "kitty",
            "com.mitchellh.ghostty",
            "org.wezfurlong.wezterm",
        ] {
            assert!(is_terminal(class, &[], &[]), "{class}");
        }
        assert!(is_terminal("org.omarchy.btop", &[], &[]));
        assert!(is_terminal("TUI.float", &[], &[]));
    }

    #[test]
    fn omarchy_terminal_tag() {
        assert!(is_terminal("my-term", &["terminal*".into()], &[]));
        assert!(is_terminal("my-term", &["terminal".into()], &[]));
        assert!(!is_terminal("my-term", &["pinned".into()], &[]));
    }

    #[test]
    fn other_apps_are_not_terminals() {
        for class in [
            "brave-browser",
            "com.t3tools.T3Code",
            "code",
            "firefox",
            "org.gnome.Nautilus",
            "",
        ] {
            assert!(!is_terminal(class, &[], &[]), "{class}");
        }
    }

    #[test]
    fn configured_terminals() {
        let extra = vec!["MyTerm".to_string()];
        assert!(is_terminal("myterm", &[], &extra));
        assert!(!is_terminal("other", &[], &extra));
    }
}
