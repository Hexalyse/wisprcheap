//! Tray icon, context menu, log window and error notifications, on the main thread's event loop.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tokio::sync::mpsc::UnboundedSender;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::icons::{IconName, render_rgba, write_icons};

#[cfg(windows)]
#[path = "logwin_windows.rs"]
mod logwin;
#[cfg(target_os = "linux")]
#[path = "logwin_gtk.rs"]
mod logwin;

mod notify;

/// Lines kept in the log window (also while it's hidden).
pub const LOG_LINES: usize = 3000;

/// Messages from the app to the UI thread.
#[derive(Debug)]
pub enum UiEvent {
    State(IconName, String),
    Log(String),
    LastAvailable(bool),
    FailedAvailable(bool),
    Paused(bool),
    Month(String),
    /// Sync status line; None hides the sync items (sync off).
    Sync(Option<String>),
    Translations(Vec<String>, i32),
    Notify(String, String),
    ShowLog,
    ToggleLog,
    /// The log window was hidden by the user (Esc / close button).
    LogHidden,
    Exit,
}

/// Actions chosen in the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    CopyLast,
    RetryFailed,
    AddClipboard,
    TogglePause,
    Translate(i32),
    SyncNow,
    OpenConfig,
    Restart,
    Quit,
}

/// Handle used by the app (any thread) to drive the UI.
#[derive(Clone)]
pub struct Ui {
    proxy: Arc<Mutex<EventLoopProxy<UiEvent>>>,
}

fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

impl Ui {
    fn send(&self, event: UiEvent) {
        let _ = self.proxy.lock().unwrap().send_event(event);
    }

    pub fn set_state(&self, icon: IconName, text: &str) {
        self.send(UiEvent::State(icon, one_line(text)));
    }

    pub fn log(&self, text: &str) {
        for line in text.split('\n') {
            self.send(UiEvent::Log(line.trim_end_matches('\r').to_string()));
        }
    }

    pub fn set_last_available(&self, v: bool) {
        self.send(UiEvent::LastAvailable(v));
    }

    pub fn set_failed_available(&self, v: bool) {
        self.send(UiEvent::FailedAvailable(v));
    }

    pub fn set_paused(&self, v: bool) {
        self.send(UiEvent::Paused(v));
    }

    pub fn set_month(&self, text: &str) {
        self.send(UiEvent::Month(one_line(text)));
    }

    pub fn set_sync(&self, line: Option<String>) {
        self.send(UiEvent::Sync(line.map(|l| one_line(&l))));
    }

    pub fn set_translations(&self, labels: Vec<String>, selected: i32) {
        self.send(UiEvent::Translations(labels, selected));
    }

    pub fn notify_error(&self, title: &str, message: &str) {
        self.send(UiEvent::Notify(one_line(title), message.to_string()));
    }

    pub fn show_log(&self) {
        self.send(UiEvent::ShowLog);
    }

    /// Remove the tray icon and end the event loop (which exits the process).
    pub fn exit(&self) {
        self.send(UiEvent::Exit);
    }
}

pub fn create_event_loop() -> (EventLoop<UiEvent>, Ui) {
    let event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    let ui = Ui {
        proxy: Arc::new(Mutex::new(event_loop.create_proxy())),
    };
    (event_loop, ui)
}

struct Icons {
    idle: Icon,
    recording: Icon,
    processing: Icon,
    paused: Icon,
}

impl Icons {
    fn get(&self, name: IconName) -> Icon {
        match name {
            IconName::Idle => self.idle.clone(),
            IconName::Recording => self.recording.clone(),
            IconName::Processing => self.processing.clone(),
            IconName::Paused => self.paused.clone(),
        }
    }
}

#[cfg(windows)]
fn load_icon(dir: &std::path::Path, name: IconName) -> Icon {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
    let path = dir.join(format!("{}.ico", name.as_str()));
    Icon::from_path(&path, Some((size, size)))
        .or_else(|_| Icon::from_rgba(render_rgba(name, size), size, size))
        .expect("tray icon")
}

#[cfg(not(windows))]
fn load_icon(_dir: &std::path::Path, name: IconName) -> Icon {
    Icon::from_rgba(render_rgba(name, 64), 64, 64).expect("tray icon")
}

struct Tray {
    icon: TrayIcon,
    menu: Menu,
    icons: Icons,
    status: MenuItem,
    month: MenuItem,
    show_log: MenuItem,
    copy_last: MenuItem,
    retry: MenuItem,
    pause: CheckMenuItem,
    translate: Option<(Submenu, PredefinedMenuItem, Vec<CheckMenuItem>)>,
    /// Sync status line and "Sync now", under the month line when sync is on.
    sync: Option<(MenuItem, MenuItem)>,
}

/// Position of the "Translate dictation" submenu in the menu (without the sync items).
const TRANSLATE_POSITION: usize = 7;
/// Position of the sync status line.
const SYNC_POSITION: usize = 2;

impl Tray {
    fn new(icon_dir: &std::path::Path) -> anyhow::Result<Self> {
        let icons = Icons {
            idle: load_icon(icon_dir, IconName::Idle),
            recording: load_icon(icon_dir, IconName::Recording),
            processing: load_icon(icon_dir, IconName::Processing),
            paused: load_icon(icon_dir, IconName::Paused),
        };
        let menu = Menu::new();
        let status = MenuItem::with_id("status", "wisprcheap", false, None);
        let month = MenuItem::with_id("month", "This month: -", false, None);
        let show_log = MenuItem::with_id("show-log", "Show log", true, None);
        let copy_last = MenuItem::with_id("copy-last", "Copy last dictation", false, None);
        let retry = MenuItem::with_id("retry-failed", "Retry last failed", false, None);
        let add_clipboard =
            MenuItem::with_id("add-clipboard", "Add clipboard to dictionary", true, None);
        let pause = CheckMenuItem::with_id("toggle-pause", "Pause dictation", true, false, None);
        let open_config = MenuItem::with_id("open-config", "Open config.yaml", true, None);
        let restart = MenuItem::with_id("restart", "Restart", true, None);
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        menu.append_items(&[
            &status,
            &month,
            &PredefinedMenuItem::separator(),
            &show_log,
            &copy_last,
            &retry,
            &PredefinedMenuItem::separator(),
            // (translate submenu + separator inserted here when pairs are configured)
            &add_clipboard,
            &pause,
            &PredefinedMenuItem::separator(),
            &open_config,
            &restart,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;
        let icon = TrayIconBuilder::new()
            .with_id("wisprcheap")
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(false)
            .with_tooltip("wisprcheap")
            .with_icon(icons.get(IconName::Idle))
            .build()?;
        Ok(Self {
            icon,
            menu,
            icons,
            status,
            month,
            show_log,
            copy_last,
            retry,
            pause,
            translate: None,
            sync: None,
        })
    }

    fn translate_position(&self) -> usize {
        TRANSLATE_POSITION + if self.sync.is_some() { 2 } else { 0 }
    }

    fn set_sync(&mut self, line: Option<&str>) {
        match (line, &self.sync) {
            (Some(text), Some((status, _))) => status.set_text(text),
            (Some(text), None) => {
                let status = MenuItem::with_id("sync-status", text, false, None);
                let now = MenuItem::with_id("sync-now", "Sync now", true, None);
                let _ = self.menu.insert(&status, SYNC_POSITION);
                let _ = self.menu.insert(&now, SYNC_POSITION + 1);
                self.sync = Some((status, now));
            }
            (None, Some(_)) => {
                if let Some((status, now)) = self.sync.take() {
                    let _ = self.menu.remove(&status);
                    let _ = self.menu.remove(&now);
                }
            }
            (None, None) => {}
        }
    }

    fn set_state(&mut self, name: IconName, text: &str) {
        let _ = self.icon.set_icon(Some(self.icons.get(name)));
        let full = format!("wisprcheap: {text}");
        let tooltip: String = full.chars().take(63).collect();
        let _ = self.icon.set_tooltip(Some(&tooltip));
        self.status.set_text(&full);
    }

    fn set_translations(&mut self, labels: &[String], selected: i32) {
        if let Some((submenu, separator, _)) = self.translate.take() {
            let _ = self.menu.remove(&submenu);
            let _ = self.menu.remove(&separator);
        }
        if labels.is_empty() {
            return;
        }
        let title = match usize::try_from(selected).ok().and_then(|i| labels.get(i)) {
            Some(label) => format!("Translate dictation: {label}"),
            None => "Translate dictation".to_string(),
        };
        let submenu = Submenu::with_id("translate", title, true);
        let off = CheckMenuItem::with_id("translate:-1", "Off", true, selected < 0, None);
        let _ = submenu.append(&off);
        let _ = submenu.append(&PredefinedMenuItem::separator());
        let mut items = vec![off];
        for (i, label) in labels.iter().enumerate() {
            let item = CheckMenuItem::with_id(
                format!("translate:{i}"),
                label.replace('\t', " "),
                true,
                selected == i as i32,
                None,
            );
            let _ = submenu.append(&item);
            items.push(item);
        }
        let separator = PredefinedMenuItem::separator();
        let position = self.translate_position();
        let _ = self.menu.insert(&submenu, position);
        let _ = self.menu.insert(&separator, position + 1);
        self.translate = Some((submenu, separator, items));
    }
}

fn menu_action(id: &str) -> Option<TrayAction> {
    Some(match id {
        "copy-last" => TrayAction::CopyLast,
        "retry-failed" => TrayAction::RetryFailed,
        "add-clipboard" => TrayAction::AddClipboard,
        "toggle-pause" => TrayAction::TogglePause,
        "sync-now" => TrayAction::SyncNow,
        "open-config" => TrayAction::OpenConfig,
        "restart" => TrayAction::Restart,
        "quit" => TrayAction::Quit,
        _ => TrayAction::Translate(id.strip_prefix("translate:")?.parse().ok()?),
    })
}

/// Run the UI event loop on the main thread (never returns).
pub fn run(
    event_loop: EventLoop<UiEvent>,
    ui: Ui,
    actions: UnboundedSender<TrayAction>,
    keep_alive: impl Send + 'static,
) -> ! {
    let icon_dir: PathBuf = write_icons(&crate::paths::icon_dir());

    {
        let ui = ui.clone();
        let actions = actions.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let id = event.id.0.as_str();
            if id == "show-log" {
                ui.send(UiEvent::ToggleLog);
            } else if let Some(action) = menu_action(id) {
                let _ = actions.send(action);
            }
        }));
    }
    {
        let ui = ui.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                ui.send(UiEvent::ToggleLog);
            }
        }));
    }

    let mut tray: Option<Tray> = None;
    let mut log_window = {
        let ui = ui.clone();
        logwin::LogWindow::new(
            icon_dir.clone(),
            Box::new(move || ui.send(UiEvent::LogHidden)),
        )
    };
    let notify_ui = ui.clone();
    let mut tray_failed = false;

    event_loop.run(move |event, _target, control_flow| {
        // The app's runtime lives as long as the event loop.
        let _ = &keep_alive;
        *control_flow = ControlFlow::Wait;
        if tray.is_none() && !tray_failed {
            match Tray::new(&icon_dir) {
                Ok(t) => {
                    notify::init(&t.icon, {
                        let ui = notify_ui.clone();
                        move || ui.send(UiEvent::ShowLog)
                    });
                    tray = Some(t);
                }
                Err(e) => {
                    tray_failed = true;
                    crate::warn!(
                        "[tray] Could not create the tray icon: {e}\n  Dictation keeps working. Use `wisprcheap stop` to quit."
                    );
                }
            }
        }
        let Event::UserEvent(event) = event else {
            return;
        };
        let update_show_log = |tray: &Option<Tray>, visible: bool| {
            if let Some(t) = tray {
                t.show_log.set_text(if visible { "Hide log" } else { "Show log" });
            }
        };
        match event {
            UiEvent::State(name, text) => {
                if let Some(t) = tray.as_mut() {
                    t.set_state(name, &text);
                }
                log_window.set_title(&format!("wisprcheap log - {text}"));
            }
            UiEvent::Log(line) => log_window.append(&line),
            UiEvent::LastAvailable(v) => {
                if let Some(t) = &tray {
                    t.copy_last.set_enabled(v);
                }
            }
            UiEvent::FailedAvailable(v) => {
                if let Some(t) = &tray {
                    t.retry.set_enabled(v);
                }
            }
            UiEvent::Paused(v) => {
                if let Some(t) = &tray {
                    t.pause.set_checked(v);
                }
            }
            UiEvent::Month(text) => {
                if let Some(t) = &tray {
                    t.month.set_text(&text);
                }
            }
            UiEvent::Sync(line) => {
                if let Some(t) = tray.as_mut() {
                    t.set_sync(line.as_deref());
                }
            }
            UiEvent::Translations(labels, selected) => {
                if let Some(t) = tray.as_mut() {
                    t.set_translations(&labels, selected);
                }
            }
            UiEvent::Notify(title, message) => {
                if let Some(t) = &tray {
                    notify::show_error(&t.icon, &title, &message);
                }
            }
            UiEvent::ShowLog => {
                log_window.show();
                update_show_log(&tray, true);
            }
            UiEvent::ToggleLog => {
                let visible = log_window.toggle();
                update_show_log(&tray, visible);
            }
            UiEvent::LogHidden => update_show_log(&tray, false),
            UiEvent::Exit => {
                log_window.destroy();
                tray = None; // removes the icon
                tray_failed = true; // don't re-create it
                *control_flow = ControlFlow::Exit;
            }
        }
    })
}
