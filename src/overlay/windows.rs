//! Windows: a layered (per-pixel alpha), click-through, topmost tool window that never activates. Each frame
//! is drawn in software and handed to UpdateLayeredWindow. A timer drives the animation while the overlay is
//! on screen; nothing runs while it's hidden.

use std::cell::{Cell, RefCell};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetMonitorInfoW,
    HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint, MonitorFromWindow, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::paint::Canvas;
use super::scene::{CANVAS_H, CANVAS_W, Scene};
use super::{Feedback, OverlayStatus};

const CLASS_NAME: &str = "WisprcheapOverlay";
const TIMER_ID: usize = 1;
/// Just under the default 15.6 ms timer resolution, so every tick fires (about 64 frames per second).
const FRAME_MS: u32 = 15;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A 32-bit top-down DIB selected into a memory DC: what UpdateLayeredWindow copies from.
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Surface {
    fn new(width: i32, height: i32) -> Option<Self> {
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down rows, like the canvas
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                },
                ..std::mem::zeroed()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(
                std::ptr::null_mut(),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if bitmap.is_null() || bits.is_null() {
                return None;
            }
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                DeleteObject(bitmap);
                return None;
            }
            let previous = SelectObject(dc, bitmap);
            Some(Self {
                dc,
                bitmap,
                previous,
                bits: bits as *mut u8,
                width,
                height,
            })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteDC(self.dc);
            DeleteObject(self.bitmap);
        }
    }
}

struct State {
    hwnd: HWND,
    scene: Scene,
    canvas: Canvas,
    surface: Option<Surface>,
    /// Top-left corner on screen (physical pixels), chosen when the overlay appears.
    origin: POINT,
    /// Pixels per device-independent pixel on that monitor.
    scale: f32,
    /// Shown, with the animation timer running.
    active: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    static FAILED: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` on the overlay state, creating the window the first time.
fn with_state(f: impl FnOnce(&mut State)) {
    STATE.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return;
        };
        if slot.is_none() && !FAILED.get() {
            *slot = State::create();
            FAILED.set(slot.is_none());
        }
        if let Some(state) = slot.as_mut() {
            f(state);
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER if wparam == TIMER_ID => {
                STATE.with(|cell| {
                    if let Ok(mut slot) = cell.try_borrow_mut()
                        && let Some(state) = slot.as_mut()
                    {
                        state.frame();
                    }
                });
                0
            }
            WM_NCHITTEST => HTTRANSPARENT as LRESULT,
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// The monitor of the focused window (where the text will go), or else the one under the mouse.
unsafe fn target_monitor() -> HMONITOR {
    unsafe {
        let foreground = GetForegroundWindow();
        if !foreground.is_null() {
            return MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST);
        }
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY)
    }
}

impl State {
    fn create() -> Option<Self> {
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let class = wide(CLASS_NAME);
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);
            let title = wide("wisprcheap overlay");
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                crate::warn!(
                    "[overlay] Could not create the overlay window: {}",
                    std::io::Error::last_os_error()
                );
                return None;
            }
            Some(Self {
                hwnd,
                scene: Scene::new(Instant::now()),
                canvas: Canvas::new(0, 0),
                surface: None,
                origin: POINT { x: 0, y: 0 },
                scale: 1.0,
                active: false,
            })
        }
    }

    /// Show the window and start the animation if the scene has something to show.
    fn wake(&mut self) {
        let now = Instant::now();
        if self.active || !self.scene.on_screen(now) {
            return;
        }
        self.place();
        self.scene.tick(now, crate::recorder::take_level());
        if !self.present(now) {
            return;
        }
        unsafe {
            ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            // back above other topmost windows that appeared since
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            );
            SetTimer(self.hwnd, TIMER_ID, FRAME_MS, None);
        }
        self.active = true;
    }

    fn frame(&mut self) {
        let now = Instant::now();
        self.scene.tick(now, crate::recorder::take_level());
        if self.scene.on_screen(now) {
            self.present(now);
            return;
        }
        unsafe {
            KillTimer(self.hwnd, TIMER_ID);
            ShowWindow(self.hwnd, SW_HIDE);
        }
        self.active = false;
    }

    /// Bottom center of the work area (above the taskbar), sized for the monitor's DPI.
    fn place(&mut self) {
        unsafe {
            let monitor = target_monitor();
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            let area = if GetMonitorInfoW(monitor, &mut info) != 0 {
                info.rcWork
            } else {
                let mut area: RECT = std::mem::zeroed();
                SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
                area
            };
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y) != 0 {
                dpi = 96;
            }
            let scale = dpi.max(96) as f32 / 96.0;
            let (w, h) = (
                (CANVAS_W * scale).ceil() as i32,
                (CANVAS_H * scale).ceil() as i32,
            );
            if self
                .surface
                .as_ref()
                .is_none_or(|s| (s.width, s.height) != (w, h))
            {
                self.surface = Surface::new(w, h);
            }
            self.canvas.resize(w as usize, h as usize);
            self.scale = scale;
            self.origin = POINT {
                x: area.left + (area.right - area.left - w) / 2,
                y: area.bottom - h,
            };
        }
    }

    /// Draw the frame and put it on screen.
    fn present(&mut self, now: Instant) -> bool {
        let Some(surface) = &self.surface else {
            return false;
        };
        self.scene.draw(&mut self.canvas, self.scale, now);
        let bytes = (surface.width * surface.height * 4) as usize;
        if self.canvas.pixels.len() != bytes {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(self.canvas.pixels.as_ptr(), surface.bits, bytes);
            let size = SIZE {
                cx: surface.width,
                cy: surface.height,
            };
            let source = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(
                self.hwnd,
                std::ptr::null_mut(),
                &self.origin,
                &size,
                surface.dc,
                &source,
                0,
                &blend,
                ULW_ALPHA,
            ) != 0
        }
    }
}

/// The overlay, owned by the UI thread. The window and its state live in a thread-local, where the window
/// procedure finds them.
#[derive(Default)]
pub struct Overlay(());

impl Overlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_status(&mut self, status: OverlayStatus) {
        let exists = STATE.with(|cell| cell.try_borrow().is_ok_and(|s| s.is_some()));
        if !status.enabled && !exists {
            return; // no window until it's needed
        }
        with_state(|state| {
            state.scene.set_status(status);
            state.wake();
        });
    }

    pub fn feedback(&mut self, feedback: Feedback) {
        with_state(|state| {
            state.scene.feedback(feedback, Instant::now());
            state.wake();
        });
    }

    pub fn destroy(&mut self) {
        let Some(state) = STATE.with(|cell| cell.try_borrow_mut().ok().and_then(|mut s| s.take()))
        else {
            return;
        };
        unsafe {
            KillTimer(state.hwnd, TIMER_ID);
            DestroyWindow(state.hwnd);
        }
    }
}
