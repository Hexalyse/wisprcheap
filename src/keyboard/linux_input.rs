//! Linux kernel input devices, for Wayland (they work under X11 too). Keyboards are read from
//! `/dev/input/event*` without grabbing them, so keys still reach the focused app. Ctrl+C / Ctrl+V are
//! sent through a uinput virtual keyboard, which the compositor treats like a real one.
//! This needs read access to the keyboards and write access to `/dev/uinput` (see `wisprcheap wayland`).

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, Device, EventType, InputEvent, KeyCode as EvKey};
use tokio::sync::mpsc::UnboundedSender;

use super::{KeyEvent, Letter};
use crate::hotkey::{KeyCode, key_code, keys};

/// Name of our virtual keyboard; devices with this name are never read (our own keystrokes).
pub const VIRTUAL_NAME: &str = "wisprcheap virtual keyboard";
const INPUT_DIR: &str = "/dev/input";
pub const UINPUT_PATH: &str = "/dev/uinput";

/// Mouse, joystick and touch buttons share the key event type; they're not keyboard keys.
fn is_button(code: u16) -> bool {
    (0x100..0x160).contains(&code) || (0x2c0..0x300).contains(&code)
}

/// A device with at least one keyboard key (keyboards, media keys, power button...), except ours.
fn is_keyboard(device: &Device) -> bool {
    device.name() != Some(VIRTUAL_NAME)
        && device
            .supported_keys()
            .is_some_and(|k| k.iter().any(|c| c.code() < 0x100))
}

fn event_nodes() -> Vec<PathBuf> {
    let mut nodes: Vec<PathBuf> = std::fs::read_dir(INPUT_DIR)
        .map(|dir| {
            dir.flatten()
                .map(|e| e.path())
                .filter(|p| is_event_node(p))
                .collect()
        })
        .unwrap_or_default();
    nodes.sort();
    nodes
}

fn is_event_node(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("event"))
}

// ---------------------------------------------------------------------------
// Access check (startup and `wisprcheap wayland`)
// ---------------------------------------------------------------------------

pub struct Probe {
    /// Keyboards we can read: (node, name).
    pub keyboards: Vec<(PathBuf, String)>,
    /// Input devices we can't open (their type is unknown without access).
    pub denied: usize,
    /// Whether a virtual keyboard can be created.
    pub uinput: io::Result<()>,
}

pub fn probe() -> Probe {
    let mut keyboards = Vec::new();
    let mut denied = 0;
    for node in event_nodes() {
        match Device::open(&node) {
            Ok(device) if is_keyboard(&device) => {
                let name = device.name().unwrap_or("unnamed keyboard").to_string();
                keyboards.push((node, name));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => denied += 1,
            Err(_) => {}
        }
    }
    let uinput = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(UINPUT_PATH)
        .map(|_| ());
    Probe {
        keyboards,
        denied,
        uinput,
    }
}

/// Why the virtual keyboard can't be created, in plain words.
pub fn uinput_problem(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => {
            format!("{UINPUT_PATH} doesn't exist (the uinput kernel module isn't loaded)")
        }
        io::ErrorKind::PermissionDenied => format!("no permission to use {UINPUT_PATH}"),
        _ => format!("can't use {UINPUT_PATH}: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Listening
// ---------------------------------------------------------------------------

struct Shared {
    tx: UnboundedSender<KeyEvent>,
    /// Nodes being read (a device can be reported twice by the hotplug watcher).
    open: Mutex<HashSet<PathBuf>>,
    stopped: AtomicBool,
}

impl Shared {
    fn send(&self, code: u16, down: bool) {
        if !self.stopped.load(Ordering::Relaxed) {
            let _ = self.tx.send(KeyEvent {
                code: to_key_code(code),
                down,
            });
        }
    }
}

pub struct Listener {
    shared: Arc<Shared>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Listener {
    /// Read every keyboard now connected, and the ones connected later.
    /// Returns the listener and the number of keyboards opened.
    pub fn start(tx: UnboundedSender<KeyEvent>) -> io::Result<(Self, usize)> {
        let shared = Arc::new(Shared {
            tx,
            open: Mutex::new(HashSet::new()),
            stopped: AtomicBool::new(false),
        });
        let mut opened = 0;
        for node in event_nodes() {
            if let Ok(true) = open_device(&shared, &node, false) {
                opened += 1;
            }
        }
        let watcher = match watch_hotplug(&shared) {
            Ok(w) => Some(w),
            Err(e) => {
                crate::warn!("[hotkey] Keyboards connected from now on won't be seen: {e}");
                None
            }
        };
        Ok((
            Self {
                shared,
                _watcher: watcher,
            },
            opened,
        ))
    }

    /// The reading threads end at their next key event (a blocking read can't be interrupted).
    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
    }
}

/// Start reading `node` if it's a keyboard we don't read yet. Returns whether it was opened.
fn open_device(shared: &Arc<Shared>, node: &Path, announce: bool) -> io::Result<bool> {
    if shared.open.lock().unwrap().contains(node) {
        return Ok(false);
    }
    let device = Device::open(node)?;
    if !is_keyboard(&device) {
        return Ok(false);
    }
    if !shared.open.lock().unwrap().insert(node.to_path_buf()) {
        return Ok(false);
    }
    let name = device.name().unwrap_or("unnamed keyboard").to_string();
    if announce {
        crate::info!("[hotkey] Keyboard connected: {name}");
    }
    let shared2 = shared.clone();
    let node2 = node.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("keyboard-evdev".into())
        .spawn(move || read_device(shared2, node2, device, name));
    if let Err(e) = spawned {
        shared.open.lock().unwrap().remove(node);
        return Err(e);
    }
    Ok(true)
}

fn read_device(shared: Arc<Shared>, node: PathBuf, mut device: Device, name: String) {
    let mut down: HashSet<u16> = HashSet::new();
    loop {
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(e) => {
                // ENODEV: unplugged. Anything else is unexpected.
                if e.raw_os_error() != Some(19) && !shared.stopped.load(Ordering::Relaxed) {
                    crate::warn!("[hotkey] Stopped reading {name}: {e}");
                }
                break;
            }
        };
        if shared.stopped.load(Ordering::Relaxed) {
            break;
        }
        for event in events {
            if event.event_type() != EventType::KEY || is_button(event.code()) {
                continue;
            }
            let code = event.code();
            match event.value() {
                1 => {
                    down.insert(code);
                    shared.send(code, true);
                }
                0 => {
                    down.remove(&code);
                    shared.send(code, false);
                }
                _ => {} // auto-repeat
            }
        }
    }
    // Unplugged with keys held: release them so the hotkey isn't stuck.
    for code in down {
        shared.send(code, false);
    }
    shared.open.lock().unwrap().remove(&node);
}

/// Open keyboards as they're connected. udev sets the permissions right after creating the node,
/// so a node that isn't readable yet is retried for a moment.
fn watch_hotplug(shared: &Arc<Shared>) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher};
    let shared = Arc::downgrade(shared);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            return;
        }
        let Some(shared) = shared.upgrade() else {
            return;
        };
        for node in event.paths.into_iter().filter(|p| is_event_node(p)) {
            let shared = shared.clone();
            let _ = std::thread::Builder::new()
                .name("keyboard-hotplug".into())
                .spawn(move || {
                    for _ in 0..15 {
                        if shared.stopped.load(Ordering::Relaxed) {
                            return;
                        }
                        match open_device(&shared, &node, true) {
                            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                                std::thread::sleep(Duration::from_millis(200));
                            }
                            _ => return,
                        }
                    }
                });
        }
    })?;
    watcher.watch(Path::new(INPUT_DIR), RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

/// evdev key code -> libuiohook key code (the codes used by the hotkey config).
pub fn to_key_code(code: u16) -> KeyCode {
    let named = |name: &str| key_code(name).unwrap_or(keys::UNKNOWN_BASE);
    match code {
        // The main block uses the PC scan codes, like libuiohook.
        1..=83 | 87 | 88 => KeyCode::from(code),
        96 => named("NumpadEnter"),
        97 => keys::CTRL_RIGHT,
        98 => named("NumpadDivide"),
        99 => named("PrintScreen"),
        100 => keys::ALT_RIGHT,
        102 => named("Home"),
        103 => named("ArrowUp"),
        104 => named("PageUp"),
        105 => named("ArrowLeft"),
        106 => named("ArrowRight"),
        107 => named("End"),
        108 => named("ArrowDown"),
        109 => named("PageDown"),
        110 => named("Insert"),
        111 => named("Delete"),
        125 => keys::META,
        126 => keys::META_RIGHT,
        183..=194 => named(&format!("F{}", code - 170)),
        _ => keys::UNKNOWN_BASE + KeyCode::from(code),
    }
}

// ---------------------------------------------------------------------------
// Injection
// ---------------------------------------------------------------------------

pub struct Injector(Mutex<VirtualDevice>);

impl Injector {
    /// Create the virtual keyboard. Done at startup: the compositor needs a moment to add a new
    /// device, and keys sent before that are lost.
    pub fn new() -> io::Result<Self> {
        // A full set of keys, so it's classified as a keyboard (udev's ID_INPUT_KEYBOARD).
        let mut supported = AttributeSet::<EvKey>::new();
        for code in 1..=248u16 {
            supported.insert(EvKey::new(code));
        }
        let device = VirtualDevice::builder()?
            .name(VIRTUAL_NAME)
            .with_keys(&supported)?
            .build()?;
        Ok(Self(Mutex::new(device)))
    }

    /// Press and release Ctrl+`letter`. The key is the one at that position on a QWERTY keyboard,
    /// which the compositor translates with the current layout (the same letter on AZERTY, QWERTZ, Colemak).
    pub fn tap_ctrl(&self, letter: Letter) -> io::Result<()> {
        let key = match letter {
            Letter::C => EvKey::KEY_C,
            Letter::V => EvKey::KEY_V,
        };
        let ctrl = EvKey::KEY_LEFTCTRL;
        let mut device = self.0.lock().unwrap();
        for (code, value) in [(ctrl, 1), (key, 1), (key, 0), (ctrl, 0)] {
            device.emit(&[InputEvent::new(EventType::KEY.0, code.0, value)])?;
            std::thread::sleep(Duration::from_millis(8));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes_match_the_hotkey_names() {
        let cases = [
            (EvKey::KEY_V, "V"),
            (EvKey::KEY_C, "C"),
            (EvKey::KEY_ESC, "Escape"),
            (EvKey::KEY_SPACE, "Space"),
            (EvKey::KEY_F1, "F1"),
            (EvKey::KEY_F12, "F12"),
            (EvKey::KEY_F13, "F13"),
            (EvKey::KEY_F24, "F24"),
            (EvKey::KEY_LEFTCTRL, "Ctrl"),
            (EvKey::KEY_RIGHTCTRL, "CtrlRight"),
            (EvKey::KEY_LEFTALT, "Alt"),
            (EvKey::KEY_RIGHTALT, "AltRight"),
            (EvKey::KEY_LEFTSHIFT, "Shift"),
            (EvKey::KEY_RIGHTSHIFT, "ShiftRight"),
            (EvKey::KEY_LEFTMETA, "Meta"),
            (EvKey::KEY_RIGHTMETA, "MetaRight"),
            (EvKey::KEY_DELETE, "Delete"),
            (EvKey::KEY_UP, "ArrowUp"),
            (EvKey::KEY_KP0, "Numpad0"),
            (EvKey::KEY_KPENTER, "NumpadEnter"),
            (EvKey::KEY_SYSRQ, "PrintScreen"),
        ];
        for (ev, name) in cases {
            assert_eq!(to_key_code(ev.0), key_code(name).unwrap(), "{name}");
        }
    }

    #[test]
    fn unknown_keys_never_match_a_name() {
        for code in [84u16, 85, 86, 89, 95, 113, 114, 115, 127, 240] {
            assert!(to_key_code(code) >= keys::UNKNOWN_BASE, "{code}");
        }
    }

    #[test]
    fn buttons_are_not_keys() {
        assert!(is_button(EvKey::BTN_LEFT.0));
        assert!(is_button(EvKey::BTN_TOUCH.0));
        assert!(!is_button(EvKey::KEY_A.0));
        assert!(!is_button(EvKey::KEY_OK.0));
    }

    /// Needs write access to /dev/uinput and read access to the new /dev/input nodes (e.g. as root):
    /// `cargo test -- --ignored virtual_keyboards`.
    #[test]
    #[ignore]
    fn virtual_keyboards_end_to_end() {
        use std::time::Instant;
        use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

        fn fake_keyboard(name: &str) -> VirtualDevice {
            let mut supported = AttributeSet::<EvKey>::new();
            for code in 1..=248u16 {
                supported.insert(EvKey::new(code));
            }
            VirtualDevice::builder()
                .unwrap()
                .name(name)
                .with_keys(&supported)
                .unwrap()
                .build()
                .unwrap()
        }
        fn press(device: &mut VirtualDevice, key: EvKey, value: i32) {
            device
                .emit(&[InputEvent::new(EventType::KEY.0, key.0, value)])
                .unwrap();
        }
        fn collect(rx: &mut UnboundedReceiver<KeyEvent>, wait: Duration) -> Vec<(KeyCode, bool)> {
            let deadline = Instant::now() + wait;
            let mut got = Vec::new();
            while Instant::now() < deadline {
                while let Ok(e) = rx.try_recv() {
                    got.push((e.code, e.down));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            got
        }
        let ctrl = key_code("Ctrl").unwrap();
        let v = key_code("V").unwrap();
        let f13 = key_code("F13").unwrap();

        let mut first = fake_keyboard("wisprcheap test keyboard 1");
        std::thread::sleep(Duration::from_millis(300)); // let the node appear
        let (tx, mut rx) = unbounded_channel();
        let (listener, opened) = Listener::start(tx).unwrap();
        assert!(opened >= 1, "the test keyboard wasn't opened");

        // Keys from a keyboard present at startup; auto-repeat is ignored.
        press(&mut first, EvKey::KEY_LEFTCTRL, 1);
        press(&mut first, EvKey::KEY_LEFTCTRL, 2);
        press(&mut first, EvKey::KEY_LEFTCTRL, 0);
        assert_eq!(
            collect(&mut rx, Duration::from_millis(300)),
            [(ctrl, true), (ctrl, false)]
        );

        // Our own virtual keyboard is never read.
        let injector = Injector::new().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        injector.tap_ctrl(Letter::V).unwrap();
        assert_eq!(collect(&mut rx, Duration::from_millis(500)), []);

        // A keyboard connected later is picked up; unplugging it releases the keys it held.
        let mut second = fake_keyboard("wisprcheap test keyboard 2");
        std::thread::sleep(Duration::from_millis(800));
        press(&mut second, EvKey::KEY_F13, 1);
        press(&mut second, EvKey::KEY_V, 1);
        assert_eq!(
            collect(&mut rx, Duration::from_millis(300)),
            [(f13, true), (v, true)]
        );
        drop(second);
        let mut released = collect(&mut rx, Duration::from_millis(500));
        released.sort();
        let mut expected = vec![(f13, false), (v, false)];
        expected.sort();
        assert_eq!(released, expected);

        listener.stop();
        press(&mut first, EvKey::KEY_A, 1);
        press(&mut first, EvKey::KEY_A, 0);
        assert_eq!(collect(&mut rx, Duration::from_millis(300)), []);
    }
}
