//! Error notifications; clicking one opens the log window.
//!
//! Windows: a balloon notification from the tray icon itself (shown as a toast on Windows 10/11),
//! with a WinRT toast as fallback. Linux: a freedesktop notification (D-Bus).

use std::sync::OnceLock;

use tray_icon::TrayIcon;

type OnClick = Box<dyn Fn() + Send + Sync>;
static ON_CLICK: OnceLock<OnClick> = OnceLock::new();

fn clicked() {
    if let Some(f) = ON_CLICK.get() {
        f();
    }
}

/// Cut long messages like the original (balloon text is limited to 256 characters).
fn shorten(message: &str) -> String {
    if message.chars().count() > 250 {
        let head: String = message.chars().take(247).collect();
        format!("{head}...")
    } else if message.is_empty() {
        " ".to_string()
    } else {
        message.to_string()
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Shell::{
        DefSubclassProc, NIF_INFO, NIIF_ERROR, NIM_MODIFY, NOTIFYICONDATAW, SetWindowSubclass,
        Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_USER;

    use super::*;

    /// tray-icon's callback message for its notification icon.
    const WM_USER_TRAYICON: u32 = 6002;
    const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if msg == WM_USER_TRAYICON && (lparam as u32 & 0xFFFF) == NIN_BALLOONUSERCLICK {
            clicked();
        }
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    pub fn init(tray: &TrayIcon) {
        unsafe {
            SetWindowSubclass(tray.window_handle(), Some(subclass_proc), 0x5752, 0);
        }
    }

    fn copy_wide(dst: &mut [u16], s: &str) {
        let wide: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
        dst[..wide.len()].copy_from_slice(&wide);
        dst[wide.len()] = 0;
    }

    fn balloon(tray: &TrayIcon, title: &str, message: &str) -> bool {
        unsafe {
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = tray.window_handle();
            nid.uFlags = NIF_INFO;
            nid.dwInfoFlags = NIIF_ERROR;
            nid.Anonymous.uTimeout = 8000;
            copy_wide(&mut nid.szInfoTitle, title);
            copy_wide(&mut nid.szInfo, message);
            // tray-icon numbers its icons from 1 and doesn't expose the id: find ours.
            (1..=8).any(|id| {
                nid.uID = id;
                Shell_NotifyIconW(NIM_MODIFY, &nid) != 0
            })
        }
    }

    pub fn show(tray: &TrayIcon, title: &str, message: &str) {
        if balloon(tray, title, message) {
            return;
        }
        use tauri_winrt_notification::Toast;
        let result = Toast::new(Toast::POWERSHELL_APP_ID)
            .title(title)
            .text1(message)
            .on_activated(|_| {
                clicked();
                Ok(())
            })
            .show();
        if let Err(e) = result {
            crate::warn!("[tray] could not show a notification: {e}");
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;

    pub fn init(_tray: &TrayIcon) {}

    pub fn show(_tray: &TrayIcon, title: &str, message: &str) {
        let title = title.to_string();
        let message = message.to_string();
        std::thread::spawn(move || {
            let result = notify_rust::Notification::new()
                .appname("wisprcheap")
                .summary(&title)
                .body(&message)
                .icon("dialog-error")
                .action("default", "Show log")
                .timeout(notify_rust::Timeout::Milliseconds(8000))
                .show();
            match result {
                Ok(handle) => handle.wait_for_action(|action| {
                    if action == "default" {
                        clicked();
                    }
                }),
                Err(e) => crate::warn!("[tray] could not show a notification: {e}"),
            }
        });
    }
}

pub fn init(tray: &TrayIcon, on_click: impl Fn() + Send + Sync + 'static) {
    let _ = ON_CLICK.set(Box::new(on_click));
    imp::init(tray);
}

pub fn show_error(tray: &TrayIcon, title: &str, message: &str) {
    let title = if title.is_empty() {
        "wisprcheap"
    } else {
        title
    };
    imp::show(tray, title, &shorten(message));
}
