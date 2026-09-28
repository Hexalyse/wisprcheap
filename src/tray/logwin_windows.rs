//! Windows log window: a plain Win32 window with a read-only multiline EDIT control
//! (Consolas 10 pt, dark theme). Esc or the close button hide it.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DeleteObject, FF_MODERN, FIXED_PITCH, FW_NORMAL, HBRUSH, HDC, HFONT, OUT_DEFAULT_PRECIS,
    SetBkColor, SetTextColor,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{EM_REPLACESEL, EM_SCROLLCARET, EM_SETLIMITTEXT, EM_SETSEL};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: &str = "WisprcheapLogWindow";
const MAX_CHARS: usize = 2_000_000;
const EDIT_STYLE: u32 =
    WS_CHILD | WS_VISIBLE | WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | ((g as u32) << 8) | ((b as u32) << 16)
}

struct WndState {
    edit: HWND,
    brush: HBRUSH,
    on_hidden: Box<dyn Fn()>,
}

thread_local! {
    static STATE: RefCell<Option<WndState>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&WndState) -> R) -> Option<R> {
    STATE.with(|s| s.borrow().as_ref().map(f))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                let (w, h) = ((lparam & 0xFFFF) as i32, ((lparam >> 16) & 0xFFFF) as i32);
                if let Some(edit) = with_state(|s| s.edit) {
                    MoveWindow(edit, 0, 0, w, h, 1);
                }
                0
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                STATE.with(|s| {
                    if let Some(s) = s.borrow().as_ref() {
                        (s.on_hidden)();
                    }
                });
                0
            }
            WM_SETFOCUS => {
                if let Some(edit) = with_state(|s| s.edit) {
                    SetFocus(edit);
                }
                0
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
                let hdc = wparam as HDC;
                SetTextColor(hdc, rgb(228, 228, 231));
                SetBkColor(hdc, rgb(24, 24, 27));
                with_state(|s| s.brush as LRESULT).unwrap_or(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Esc in the text box hides the window.
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        if msg == WM_KEYDOWN && wparam == VK_ESCAPE as usize {
            PostMessageW(GetParent(hwnd), WM_CLOSE, 0, 0);
            return 0;
        }
        if msg == WM_CHAR && wparam == 0x1B {
            return 0; // no beep
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }
}

pub struct LogWindow {
    hwnd: HWND,
    edit: HWND,
    font: HFONT,
    lines: VecDeque<String>,
    chars: usize,
    title: String,
    icon_dir: PathBuf,
    on_hidden: Option<Box<dyn Fn()>>,
}

impl LogWindow {
    pub fn new(icon_dir: PathBuf, on_hidden: Box<dyn Fn()>) -> Self {
        Self {
            hwnd: std::ptr::null_mut(),
            edit: std::ptr::null_mut(),
            font: std::ptr::null_mut(),
            lines: VecDeque::new(),
            chars: 0,
            title: "wisprcheap log".into(),
            icon_dir,
            on_hidden: Some(on_hidden),
        }
    }

    fn all_text(&self) -> String {
        self.lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    fn create(&mut self) {
        if !self.hwnd.is_null() {
            return;
        }
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let class = wide(CLASS_NAME);
            let brush = CreateSolidBrush(rgb(24, 24, 27));
            let icon_path = wide(&self.icon_dir.join("idle.ico").to_string_lossy());
            let load = |size: i32| {
                LoadImageW(
                    std::ptr::null_mut(),
                    icon_path.as_ptr(),
                    IMAGE_ICON,
                    size,
                    size,
                    LR_LOADFROMFILE,
                ) as HICON
            };
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                hIcon: load(GetSystemMetrics(SM_CXICON)),
                hIconSm: load(GetSystemMetrics(SM_CXSMICON)),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: brush,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);

            // 55% of the primary work area, centered.
            let mut area: RECT = std::mem::zeroed();
            SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
            let (aw, ah) = (area.right - area.left, area.bottom - area.top);
            let (w, h) = (aw * 55 / 100, ah * 55 / 100);
            let (x, y) = (area.left + (aw - w) / 2, area.top + (ah - h) / 2);

            let title = wide(&self.title);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                x,
                y,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                crate::warn!(
                    "[tray] could not create the log window: {}",
                    std::io::Error::last_os_error()
                );
                return;
            }
            let edit_class = wide("EDIT");
            let empty = wide("");
            let edit = CreateWindowExW(
                0,
                edit_class.as_ptr(),
                empty.as_ptr(),
                EDIT_STYLE,
                0,
                0,
                w,
                h,
                hwnd,
                1 as _,
                hinstance,
                std::ptr::null(),
            );
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let face = wide("Consolas");
            self.font = CreateFontW(
                -(10 * dpi / 72),
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (FIXED_PITCH | FF_MODERN) as u32,
                face.as_ptr(),
            );
            SendMessageW(edit, WM_SETFONT, self.font as WPARAM, 1);
            SendMessageW(edit, EM_SETLIMITTEXT, 0, 0);
            SetWindowSubclass(edit, Some(edit_proc), 1, 0);

            STATE.with(|s| {
                *s.borrow_mut() = Some(WndState {
                    edit,
                    brush,
                    on_hidden: self.on_hidden.take().unwrap_or_else(|| Box::new(|| {})),
                })
            });
            self.hwnd = hwnd;
            self.edit = edit;
            self.set_all_text();
        }
    }

    fn set_all_text(&mut self) {
        let text = self.all_text();
        self.chars = text.encode_utf16().count();
        let w = wide(&text);
        unsafe {
            SetWindowTextW(self.edit, w.as_ptr());
        }
        self.scroll_to_end();
    }

    fn scroll_to_end(&self) {
        unsafe {
            let len = GetWindowTextLengthW(self.edit) as usize;
            SendMessageW(self.edit, EM_SETSEL, len, len as isize);
            SendMessageW(self.edit, EM_SCROLLCARET, 0, 0);
        }
    }

    pub fn append(&mut self, line: &str) {
        self.lines.push_back(line.to_string());
        while self.lines.len() > super::LOG_LINES {
            self.lines.pop_front();
        }
        if self.edit.is_null() {
            return;
        }
        let piece = if self.chars == 0 {
            line.to_string()
        } else {
            format!("\r\n{line}")
        };
        self.chars += piece.encode_utf16().count();
        if self.chars > MAX_CHARS {
            self.set_all_text();
            return;
        }
        let w = wide(&piece);
        unsafe {
            let len = GetWindowTextLengthW(self.edit) as usize;
            SendMessageW(self.edit, EM_SETSEL, len, len as isize);
            SendMessageW(self.edit, EM_REPLACESEL, 0, w.as_ptr() as LPARAM);
        }
    }

    pub fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
        if !self.hwnd.is_null() {
            let w = wide(title);
            unsafe {
                SetWindowTextW(self.hwnd, w.as_ptr());
            }
        }
    }

    pub fn is_visible(&self) -> bool {
        !self.hwnd.is_null()
            && unsafe { IsWindowVisible(self.hwnd) != 0 && IsIconic(self.hwnd) == 0 }
    }

    pub fn show(&mut self) {
        self.create();
        if self.hwnd.is_null() {
            return;
        }
        unsafe {
            ShowWindow(self.hwnd, SW_SHOWNORMAL);
            SetForegroundWindow(self.hwnd);
            SetFocus(self.edit);
        }
        self.scroll_to_end();
    }

    pub fn hide(&mut self) {
        if !self.hwnd.is_null() {
            unsafe {
                ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Show or hide; returns whether the window is now visible.
    pub fn toggle(&mut self) -> bool {
        if self.is_visible() {
            self.hide();
            false
        } else {
            self.show();
            true
        }
    }

    pub fn destroy(&mut self) {
        if self.hwnd.is_null() {
            return;
        }
        unsafe {
            DestroyWindow(self.hwnd);
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
        }
        self.hwnd = std::ptr::null_mut();
        self.edit = std::ptr::null_mut();
    }
}
