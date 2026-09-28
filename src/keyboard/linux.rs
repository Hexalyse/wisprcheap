//! Linux: chooses how to watch the keyboard and send Ctrl+C / Ctrl+V.
//!
//! - X11 sessions: XRecord / XTest (`x11`), no setup needed.
//! - Wayland sessions: the kernel input devices (`linux_input`) when the user has access to them,
//!   since Wayland doesn't expose global keys to apps. Otherwise X11 through XWayland, which only
//!   sees and types into XWayland windows.
//!
//! `WISPRCHEAP_KEYBOARD=x11` or `=evdev` forces one.

use std::sync::OnceLock;

use tokio::sync::mpsc::UnboundedSender;

use super::linux_input::{self, Injector, Listener};
use super::{KeyEvent, Letter, x11};

const SETUP_HINT: &str = "run `wisprcheap wayland` for the one-time setup";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Auto,
    X11,
    Devices,
}

fn choice() -> Choice {
    match std::env::var("WISPRCHEAP_KEYBOARD")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "x11" => Choice::X11,
        "evdev" | "uinput" | "devices" => Choice::Devices,
        _ => Choice::Auto,
    }
}

pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
}

fn use_devices() -> bool {
    match choice() {
        Choice::Auto => is_wayland(),
        Choice::X11 => false,
        Choice::Devices => true,
    }
}

/// How keystrokes are sent; chosen once, when the hook starts.
static INJECTOR: OnceLock<Option<Injector>> = OnceLock::new();

pub struct Hook {
    listener: Option<Listener>,
    warning: Option<String>,
}

impl Hook {
    pub fn start(tx: UnboundedSender<KeyEvent>) -> anyhow::Result<Self> {
        if !use_devices() {
            let _ = INJECTOR.set(None);
            x11::listen(tx).map_err(|e| {
                anyhow::anyhow!("{e}: global hotkeys need an X11 session, or Wayland with access to the input devices ({SETUP_HINT})")
            })?;
            return Ok(Self {
                listener: None,
                warning: None,
            });
        }

        // The virtual keyboard first, so it exists before the keyboards are listed (it's skipped by name).
        let mut problems = Vec::new();
        let injector = match Injector::new() {
            Ok(i) => Some(i),
            Err(e) => {
                problems.push(linux_input::uinput_problem(&e));
                None
            }
        };
        let pastes_everywhere = injector.is_some();
        let _ = INJECTOR.set(injector);

        let listener = match Listener::start(tx.clone()) {
            Ok((listener, opened)) => {
                let denied = linux_input::probe().denied;
                if opened == 0 && denied > 0 {
                    problems.insert(
                        0,
                        "no permission to read the keyboards in /dev/input".into(),
                    );
                    listener.stop();
                    None
                } else {
                    crate::info!(
                        "[hotkey] Reading {opened} keyboard(s) from /dev/input{}",
                        if pastes_everywhere {
                            ", typing through a virtual keyboard"
                        } else {
                            ""
                        }
                    );
                    Some(listener)
                }
            }
            Err(e) => {
                problems.insert(0, format!("can't read /dev/input: {e}"));
                None
            }
        };

        let hears_everywhere = listener.is_some();
        if !hears_everywhere {
            x11::listen(tx)
                .map_err(|e| anyhow::anyhow!("{}, and {e}: {SETUP_HINT}", problems.join(", ")))?;
        }

        let warning = (!problems.is_empty()).then(|| {
            let limited = match (hears_everywhere, pastes_everywhere) {
                (false, false) => "the hotkeys and pasting only work in X11 (XWayland) apps",
                (false, true) => "the hotkeys only work while an X11 (XWayland) app has focus",
                _ => "pasting only works in X11 (XWayland) apps",
            };
            format!(
                "{}: {limited}. To fix it, {SETUP_HINT}.",
                capitalize(&problems.join(", "))
            )
        });
        Ok(Self { listener, warning })
    }

    pub fn stop(&self) {
        if let Some(listener) = &self.listener {
            listener.stop();
        }
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn tap_ctrl(letter: Letter) {
    match INJECTOR.get() {
        Some(Some(injector)) => {
            if let Err(e) = injector.tap_ctrl(letter) {
                crate::warn!("[output] could not send a key: {e}");
            }
        }
        _ => x11::tap_ctrl(letter),
    }
}

// ---------------------------------------------------------------------------
// `wisprcheap wayland`: check the access and explain the setup
// ---------------------------------------------------------------------------

const UDEV_RULES: &str = r#"sudo tee /etc/udev/rules.d/70-wisprcheap.rules <<'EOF'
SUBSYSTEM=="input", ENV{ID_INPUT_KEYBOARD}=="1", TAG+="uaccess"
KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"
EOF"#;

const GROUP_RULES: &str = r#"sudo usermod -aG input "$USER"
sudo tee /etc/udev/rules.d/70-wisprcheap.rules <<'EOF'
KERNEL=="uinput", SUBSYSTEM=="misc", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"
EOF"#;

const LOAD_UINPUT: &str = "echo uinput | sudo tee /etc/modules-load.d/wisprcheap.conf
sudo modprobe uinput
sudo udevadm control --reload-rules && sudo udevadm trigger";

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Status of the keyboard access and the setup instructions. Returns (report, everything ok).
pub fn setup_report() -> (String, bool) {
    let probe = linux_input::probe();
    let mut out = String::new();
    let session = if is_wayland() { "Wayland" } else { "X11" };
    let method = match choice() {
        Choice::Auto if is_wayland() => "input devices (automatic, Wayland session)".to_string(),
        Choice::Auto => "X11 (automatic, X11 session)".to_string(),
        Choice::X11 => "X11 (WISPRCHEAP_KEYBOARD=x11)".to_string(),
        Choice::Devices => "input devices (WISPRCHEAP_KEYBOARD)".to_string(),
    };
    out.push_str(&format!("Session:  {session}\nKeyboard: {method}\n\n"));

    let keyboards_ok = !probe.keyboards.is_empty();
    if keyboards_ok {
        out.push_str(&format!(
            "[ok] Keyboards readable: {}\n",
            probe
                .keyboards
                .iter()
                .map(|(_, name)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    } else if probe.denied > 0 {
        out.push_str(&format!(
            "[missing] No readable keyboard ({} input device(s) in /dev/input without permission)\n",
            probe.denied
        ));
    } else {
        out.push_str("[missing] No keyboard found in /dev/input\n");
    }
    let uinput_ok = probe.uinput.is_ok();
    match &probe.uinput {
        Ok(()) => out.push_str(&format!(
            "[ok] Virtual keyboard: {} is usable\n",
            linux_input::UINPUT_PATH
        )),
        Err(e) => out.push_str(&format!(
            "[missing] Virtual keyboard: {}\n",
            linux_input::uinput_problem(e)
        )),
    }

    let ok = keyboards_ok && uinput_ok;
    if ok {
        out.push_str(
            "\nAll set: on Wayland, the hotkeys and pasting work in every app.\n\
             Restart wisprcheap if it's running (tray > Restart).",
        );
        return (out, true);
    }

    out.push_str(&format!(
        "\nOn Wayland, apps can't see global keys or type into other windows. wisprcheap reads the\n\
         keyboards from /dev/input instead, and types through a virtual keyboard (/dev/uinput).\n\
         One-time setup, pick one:\n\n\
         Option 1 (recommended): a udev rule. Only the user sitting at the computer gets access,\n\
         nothing to log out of.\n\n{}\n{}\n\n\
         Option 2: the input group. Simpler, but the access applies to all your sessions (SSH too),\n\
         and it takes effect after logging out and back in.\n\n{}\n{}\n\n\
         Then restart wisprcheap (tray > Restart) and run `wisprcheap wayland` again to check.\n\n\
         Note: this lets programs running as you read keystrokes and type keys, which X11 always\n\
         allowed but Wayland normally prevents.",
        indent(UDEV_RULES),
        indent(LOAD_UINPUT),
        indent(GROUP_RULES),
        indent(LOAD_UINPUT),
    ));
    (out, false)
}
