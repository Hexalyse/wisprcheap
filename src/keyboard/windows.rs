//! Windows: WH_KEYBOARD_LL hook on a dedicated thread, SendInput for injection.

use std::sync::OnceLock;
use std::sync::mpsc;

use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW,
    SendInput, VK_CONTROL, VK_LCONTROL,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::{KeyEvent, Letter};
use crate::hotkey::{KeyCode, key_code, keys};

/// dwExtraInfo of the keystrokes we inject, so the hook ignores them.
const INJECTED_MARKER: usize = 0x5752_4348; // "WRCH"

static SENDER: OnceLock<UnboundedSender<KeyEvent>> = OnceLock::new();

pub struct Hook {
    thread_id: u32,
}

impl Hook {
    pub fn start(tx: UnboundedSender<KeyEvent>) -> anyhow::Result<Self> {
        if SENDER.set(tx).is_err() {
            anyhow::bail!("the keyboard hook is already running");
        }
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        std::thread::Builder::new()
            .name("keyboard-hook".into())
            .spawn(move || unsafe {
                let hook = SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(hook_proc),
                    GetModuleHandleW(std::ptr::null()),
                    0,
                );
                if hook.is_null() {
                    let _ = ready_tx.send(Err(std::io::Error::last_os_error().to_string()));
                    return;
                }
                let _ = ready_tx.send(Ok(GetCurrentThreadId()));
                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                UnhookWindowsHookEx(hook);
            })?;
        let thread_id = ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("the keyboard hook thread exited"))?
            .map_err(|e| anyhow::anyhow!("could not install the keyboard hook: {e}"))?;
        Ok(Self { thread_id })
    }

    pub fn stop(&self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if kb.dwExtraInfo != INJECTED_MARKER {
            let msg = wparam as u32;
            let down = match msg {
                WM_KEYDOWN | WM_SYSKEYDOWN => Some(true),
                WM_KEYUP | WM_SYSKEYUP => Some(false),
                _ => None,
            };
            if let (Some(down), Some(tx)) = (down, SENDER.get()) {
                let extended = kb.flags & LLKHF_EXTENDED != 0;
                let _ = tx.send(KeyEvent {
                    code: vk_to_code(kb.vkCode, extended),
                    down,
                });
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

fn named(name: &str) -> KeyCode {
    key_code(name).unwrap_or(keys::UNKNOWN_BASE)
}

/// Virtual-key code -> libuiohook key code.
fn vk_to_code(vk: u32, extended: bool) -> KeyCode {
    let name: Option<String> = match vk {
        0x08 => Some("Backspace".into()),
        0x09 => Some("Tab".into()),
        0x0D => Some(if extended { "NumpadEnter" } else { "Enter" }.into()),
        0x14 => Some("CapsLock".into()),
        0x1B => Some("Escape".into()),
        0x20 => Some("Space".into()),
        // Navigation keys: the numpad versions (NumLock off) are not extended.
        0x21 => Some(if extended { "PageUp" } else { "NumpadPageUp" }.into()),
        0x22 => Some(
            if extended {
                "PageDown"
            } else {
                "NumpadPageDown"
            }
            .into(),
        ),
        0x23 => Some(if extended { "End" } else { "NumpadEnd" }.into()),
        0x24 => Some(if extended { "Home" } else { "NumpadHome" }.into()),
        0x25 => Some(
            if extended {
                "ArrowLeft"
            } else {
                "NumpadArrowLeft"
            }
            .into(),
        ),
        0x26 => Some(if extended { "ArrowUp" } else { "NumpadArrowUp" }.into()),
        0x27 => Some(
            if extended {
                "ArrowRight"
            } else {
                "NumpadArrowRight"
            }
            .into(),
        ),
        0x28 => Some(
            if extended {
                "ArrowDown"
            } else {
                "NumpadArrowDown"
            }
            .into(),
        ),
        0x2C => Some("PrintScreen".into()),
        0x2D => Some(if extended { "Insert" } else { "NumpadInsert" }.into()),
        0x2E => Some(if extended { "Delete" } else { "NumpadDelete" }.into()),
        0x30..=0x39 | 0x41..=0x5A => Some(char::from_u32(vk).unwrap().to_string()),
        0x5B => return keys::META,
        0x5C => return keys::META_RIGHT,
        0x60..=0x69 => Some(format!("Numpad{}", vk - 0x60)),
        0x6A => Some("NumpadMultiply".into()),
        0x6B => Some("NumpadAdd".into()),
        0x6D => Some("NumpadSubtract".into()),
        0x6E => Some("NumpadDecimal".into()),
        0x6F => Some("NumpadDivide".into()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x6F)),
        0x90 => Some("NumLock".into()),
        0x91 => Some("ScrollLock".into()),
        0x10 | 0xA0 => return keys::SHIFT,
        0xA1 => return keys::SHIFT_RIGHT,
        0x11 => {
            return if extended {
                keys::CTRL_RIGHT
            } else {
                keys::CTRL
            };
        }
        0xA2 => return keys::CTRL,
        0xA3 => return keys::CTRL_RIGHT,
        0x12 => return if extended { keys::ALT_RIGHT } else { keys::ALT },
        0xA4 => return keys::ALT,
        0xA5 => return keys::ALT_RIGHT,
        0xBA => Some("Semicolon".into()),
        0xBB => Some("Equal".into()),
        0xBC => Some("Comma".into()),
        0xBD => Some("Minus".into()),
        0xBE => Some("Period".into()),
        0xBF => Some("Slash".into()),
        0xC0 => Some("Backquote".into()),
        0xDB => Some("BracketLeft".into()),
        0xDC => Some("Backslash".into()),
        0xDD => Some("BracketRight".into()),
        0xDE => Some("Quote".into()),
        _ => None,
    };
    match name {
        Some(n) => named(&n),
        None => keys::UNKNOWN_BASE + vk,
    }
}

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: INJECTED_MARKER,
            },
        },
    }
}

pub fn tap_ctrl(letter: Letter) {
    let vk = match letter {
        Letter::C => b'C' as u16,
        Letter::V => b'V' as u16,
    };
    let _ = VK_CONTROL;
    let inputs = [
        key_input(VK_LCONTROL, false),
        key_input(vk, false),
        key_input(vk, true),
        key_input(VK_LCONTROL, true),
    ];
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}
