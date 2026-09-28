//! Linux, X11: rdev listens through XRecord and injects through XTest.
//! Works for X11 sessions and XWayland windows; native Wayland windows don't expose global keys
//! (see `linux_input` for those).

use std::time::Duration;

use rdev::{EventType, Key};
use tokio::sync::mpsc::UnboundedSender;

use super::{KeyEvent, Letter};
use crate::hotkey::{KeyCode, key_code, keys};

/// Start listening on a background thread. rdev's listener can't be stopped; it ends with the process.
pub fn listen(tx: UnboundedSender<KeyEvent>) -> anyhow::Result<()> {
    if std::env::var_os("DISPLAY").is_none() {
        anyhow::bail!("no X11 display (DISPLAY is not set)");
    }
    std::thread::Builder::new()
        .name("keyboard-hook".into())
        .spawn(move || {
            let result = rdev::listen(move |event| {
                let (key, down) = match event.event_type {
                    EventType::KeyPress(k) => (k, true),
                    EventType::KeyRelease(k) => (k, false),
                    _ => return,
                };
                let _ = tx.send(KeyEvent {
                    code: key_to_code(key),
                    down,
                });
            });
            if let Err(e) = result {
                crate::warn!("[hotkey] The keyboard listener stopped: {e:?}");
            }
        })?;
    Ok(())
}

fn named(name: &str) -> KeyCode {
    key_code(name).unwrap_or(keys::UNKNOWN_BASE)
}

fn key_to_code(key: Key) -> KeyCode {
    use Key::*;
    let name = match key {
        Alt => return keys::ALT,
        AltGr => return keys::ALT_RIGHT,
        ControlLeft => return keys::CTRL,
        ControlRight => return keys::CTRL_RIGHT,
        ShiftLeft => return keys::SHIFT,
        ShiftRight => return keys::SHIFT_RIGHT,
        MetaLeft => return keys::META,
        MetaRight => return keys::META_RIGHT,
        Backspace => "Backspace",
        CapsLock => "CapsLock",
        Delete => "Delete",
        DownArrow => "ArrowDown",
        End => "End",
        Escape => "Escape",
        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        F5 => "F5",
        F6 => "F6",
        F7 => "F7",
        F8 => "F8",
        F9 => "F9",
        F10 => "F10",
        F11 => "F11",
        F12 => "F12",
        Home => "Home",
        LeftArrow => "ArrowLeft",
        PageDown => "PageDown",
        PageUp => "PageUp",
        Return => "Enter",
        RightArrow => "ArrowRight",
        Space => "Space",
        Tab => "Tab",
        UpArrow => "ArrowUp",
        PrintScreen => "PrintScreen",
        ScrollLock => "ScrollLock",
        NumLock => "NumLock",
        BackQuote => "Backquote",
        Num1 => "1",
        Num2 => "2",
        Num3 => "3",
        Num4 => "4",
        Num5 => "5",
        Num6 => "6",
        Num7 => "7",
        Num8 => "8",
        Num9 => "9",
        Num0 => "0",
        Minus => "Minus",
        Equal => "Equal",
        KeyQ => "Q",
        KeyW => "W",
        KeyE => "E",
        KeyR => "R",
        KeyT => "T",
        KeyY => "Y",
        KeyU => "U",
        KeyI => "I",
        KeyO => "O",
        KeyP => "P",
        LeftBracket => "BracketLeft",
        RightBracket => "BracketRight",
        KeyA => "A",
        KeyS => "S",
        KeyD => "D",
        KeyF => "F",
        KeyG => "G",
        KeyH => "H",
        KeyJ => "J",
        KeyK => "K",
        KeyL => "L",
        SemiColon => "Semicolon",
        Quote => "Quote",
        BackSlash => "Backslash",
        KeyZ => "Z",
        KeyX => "X",
        KeyC => "C",
        KeyV => "V",
        KeyB => "B",
        KeyN => "N",
        KeyM => "M",
        Comma => "Comma",
        Dot => "Period",
        Slash => "Slash",
        Insert => "Insert",
        KpReturn => "NumpadEnter",
        KpMinus => "NumpadSubtract",
        KpPlus => "NumpadAdd",
        KpMultiply => "NumpadMultiply",
        KpDivide => "NumpadDivide",
        Kp0 => "Numpad0",
        Kp1 => "Numpad1",
        Kp2 => "Numpad2",
        Kp3 => "Numpad3",
        Kp4 => "Numpad4",
        Kp5 => "Numpad5",
        Kp6 => "Numpad6",
        Kp7 => "Numpad7",
        Kp8 => "Numpad8",
        Kp9 => "Numpad9",
        KpDelete => "NumpadDecimal",
        // X keycodes of F13..F24 (evdev KEY_F13 = 183, + 8).
        Unknown(code @ 191..=202) => return named(&format!("F{}", code - 191 + 13)),
        Unknown(code) => return keys::UNKNOWN_BASE + code,
        _ => return keys::UNKNOWN_BASE + 0xFFFF,
    };
    named(name)
}

pub fn tap_ctrl(letter: Letter) {
    let key = match letter {
        Letter::C => Key::KeyC,
        Letter::V => Key::KeyV,
    };
    let events = [
        EventType::KeyPress(Key::ControlLeft),
        EventType::KeyPress(key),
        EventType::KeyRelease(key),
        EventType::KeyRelease(Key::ControlLeft),
    ];
    for event in events {
        if let Err(e) = rdev::simulate(&event) {
            crate::warn!("[output] could not send a key: {e:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
