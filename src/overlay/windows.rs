//! Windows: layered (per-pixel alpha), click-through, topmost tool windows that never activate. Each frame
//! is drawn in software and handed to UpdateLayeredWindow. A timer drives the animation while the overlay is
//! on screen; nothing runs while it's hidden.
//!
//! The pill is one window at the bottom center of the work area. The edge glow is three (`glow_paint::Piece`)
//! framing the bottom and sides of the active window; they're per-monitor DPI aware, so they line up with it
//! to the pixel and are drawn at the monitor's resolution.

use std::cell::{Cell, RefCell};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_VISIBLE_FRAME_BORDER_THICKNESS,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUNDSMALL, DwmGetWindowAttribute,
};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetMonitorInfoW,
    HBITMAP, HDC, HGDIOBJ, HMONITOR, IntersectRect, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, MonitorFromWindow, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor,
    MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::glow::GlowScene;
use super::glow_paint::{self, Piece, Target};
use super::paint::Canvas;
use super::scene::{CANVAS_H, CANVAS_W, Scene};
use super::{Feedback, OverlayStatus, OverlayStyle};

const CLASS_NAME: &str = "WisprcheapOverlay";
const TIMER_ID: usize = 1;
const GLOW_TIMER_ID: usize = 2;
/// Just under the default 15.6 ms timer resolution, so every tick fires (about 64 frames per second).
const FRAME_MS: u32 = 15;
/// How often the glow follows the focus while it's on screen.
const FOLLOW_FRAMES: u32 = 15;

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

    fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: the DIB section holds width * height 32-bit pixels and lives as long as `self`.
        unsafe {
            std::slice::from_raw_parts_mut(self.bits, (self.width * self.height * 4) as usize)
        }
    }

    /// Put the surface on screen as `hwnd`'s content, its top-left corner at `origin`.
    fn present(&self, hwnd: HWND, origin: POINT) -> bool {
        let size = SIZE {
            cx: self.width,
            cy: self.height,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                std::ptr::null_mut(),
                &origin,
                &size,
                self.dc,
                &source,
                0,
                &blend,
                ULW_ALPHA,
            ) != 0
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

/// A click-through, topmost layered window that never activates, hidden until shown.
fn create_window(title: &str) -> Option<HWND> {
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
        // fails harmlessly once the class exists
        RegisterClassExW(&wc);
        let title = wide(title);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
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
        Some(hwnd)
    }
}

/// Show a window without activating it, back above other topmost windows that appeared since.
fn show(hwnd: HWND) {
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        );
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
    static GLOW: RefCell<Option<Glow>> = const { RefCell::new(None) };
    static GLOW_FAILED: Cell<bool> = const { Cell::new(false) };
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

/// Run `f` on the edge glow, creating its windows the first time. False if it can't be shown.
fn with_glow(f: impl FnOnce(&mut Glow)) -> bool {
    GLOW.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return false;
        };
        if slot.is_none() && !GLOW_FAILED.get() {
            *slot = Glow::create();
            GLOW_FAILED.set(slot.is_none());
        }
        match slot.as_mut() {
            Some(glow) => {
                f(glow);
                true
            }
            None => false,
        }
    })
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
            WM_TIMER if wparam == GLOW_TIMER_ID => {
                GLOW.with(|cell| {
                    if let Ok(mut slot) = cell.try_borrow_mut()
                        && let Some(glow) = slot.as_mut()
                    {
                        glow.frame();
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

/// The monitor's bounds and work area (above the taskbar).
unsafe fn monitor_areas(monitor: HMONITOR) -> (RECT, RECT) {
    unsafe {
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            return (info.rcMonitor, info.rcWork);
        }
        let mut area: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
        (area, area)
    }
}

/// Physical pixels per device-independent pixel on the monitor.
unsafe fn monitor_scale(monitor: HMONITOR) -> f32 {
    let (mut dpi, mut dpi_y) = (96u32, 96u32);
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y) } != 0 {
        dpi = 96;
    }
    dpi.max(96) as f32 / 96.0
}

impl State {
    fn create() -> Option<Self> {
        let hwnd = create_window("wisprcheap overlay")?;
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
        show(self.hwnd);
        unsafe {
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
            let (_, area) = monitor_areas(monitor);
            let scale = monitor_scale(monitor);
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
        let Some(surface) = &mut self.surface else {
            return false;
        };
        self.scene.draw(&mut self.canvas, self.scale, now);
        let pixels = surface.pixels();
        if self.canvas.pixels.len() != pixels.len() {
            return false;
        }
        pixels.copy_from_slice(&self.canvas.pixels);
        surface.present(self.hwnd, self.origin)
    }
}

// ---------------------------------------------------------------------------
// Edge glow
// ---------------------------------------------------------------------------

/// Makes the thread per-monitor DPI aware while it lives: the glow's windows are created that way, and their
/// positions, and the active window's bounds, are then in physical pixels.
struct DpiAware(DPI_AWARENESS_CONTEXT);

impl DpiAware {
    fn new() -> Self {
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for DpiAware {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

/// One of the glow's windows.
struct GlowWindow {
    hwnd: HWND,
    surface: Option<Surface>,
    shown: bool,
}

impl GlowWindow {
    fn hide(&mut self) {
        if self.shown {
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
            self.shown = false;
        }
    }
}

struct Glow {
    /// In `Piece::ALL` order; the first one runs the animation timer.
    windows: [GlowWindow; 3],
    scene: GlowScene,
    /// The active window's top-left corner on screen (physical pixels), and its size.
    origin: POINT,
    target: Target,
    active: bool,
    frames: u32,
}

/// Window classes of the desktop and the taskbar: not windows to light up.
const SHELL_CLASSES: [&str; 4] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

fn is_shell(hwnd: HWND) -> bool {
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
    let name = String::from_utf16_lossy(&name[..len.max(0) as usize]);
    SHELL_CLASSES.contains(&name.as_str())
}

fn same_rect(a: &RECT, b: &RECT) -> bool {
    (a.left, a.top, a.right, a.bottom) == (b.left, b.top, b.right, b.bottom)
}

/// Radius of a window's rounded corners: Windows 11 rounds them, unless the window is maximized or asks not to.
unsafe fn corner_rounding(hwnd: HWND, bounds: &RECT, monitor: &RECT) -> f32 {
    unsafe {
        if IsZoomed(hwnd) != 0 || same_rect(bounds, monitor) {
            return 0.0;
        }
        let mut value = 0i32;
        let get = |attribute, value: &mut i32| {
            DwmGetWindowAttribute(
                hwnd,
                attribute as u32,
                value as *mut i32 as *mut _,
                size_of::<i32>() as u32,
            ) == 0
        };
        // only Windows 11 knows this attribute
        if !get(DWMWA_VISIBLE_FRAME_BORDER_THICKNESS, &mut value) {
            return 0.0;
        }
        if !get(DWMWA_WINDOW_CORNER_PREFERENCE, &mut value) {
            return 8.0;
        }
        match value {
            DWMWCP_DONOTROUND => 0.0,
            DWMWCP_ROUNDSMALL => 4.0,
            _ => 8.0,
        }
    }
}

/// What the glow frames: the focused window as it appears (without its invisible resize borders), on its
/// monitor; or, without one, the work area of the monitor under the mouse.
unsafe fn glow_target() -> (POINT, Target) {
    unsafe {
        let foreground = GetForegroundWindow();
        if !foreground.is_null()
            && IsWindowVisible(foreground) != 0
            && IsIconic(foreground) == 0
            && !is_shell(foreground)
        {
            let monitor = MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST);
            let (screen, _) = monitor_areas(monitor);
            let scale = monitor_scale(monitor);
            let mut bounds: RECT = std::mem::zeroed();
            if DwmGetWindowAttribute(
                foreground,
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                &mut bounds as *mut RECT as *mut _,
                size_of::<RECT>() as u32,
            ) != 0
            {
                GetWindowRect(foreground, &mut bounds);
            }
            let mut visible: RECT = std::mem::zeroed();
            IntersectRect(&mut visible, &bounds, &screen);
            let (w, h) = (visible.right - visible.left, visible.bottom - visible.top);
            if w as f32 >= 120.0 * scale && h as f32 >= 80.0 * scale {
                let rounding = if same_rect(&visible, &bounds) {
                    corner_rounding(foreground, &bounds, &screen)
                } else {
                    0.0
                };
                return (
                    POINT {
                        x: visible.left,
                        y: visible.top,
                    },
                    Target {
                        width: w,
                        height: h,
                        scale,
                        rounding,
                    },
                );
            }
        }
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
        let (_, area) = monitor_areas(monitor);
        (
            POINT {
                x: area.left,
                y: area.top,
            },
            Target {
                width: area.right - area.left,
                height: area.bottom - area.top,
                scale: monitor_scale(monitor),
                rounding: 0.0,
            },
        )
    }
}

impl Glow {
    fn create() -> Option<Self> {
        let _dpi = DpiAware::new();
        let mut windows = Vec::new();
        for _ in Piece::ALL {
            let Some(hwnd) = create_window("wisprcheap edge glow") else {
                for window in windows {
                    unsafe { DestroyWindow(window) };
                }
                return None;
            };
            windows.push(hwnd);
        }
        let windows = [windows[0], windows[1], windows[2]].map(|hwnd| GlowWindow {
            hwnd,
            surface: None,
            shown: false,
        });
        Some(Self {
            windows,
            scene: GlowScene::new(),
            origin: POINT { x: 0, y: 0 },
            target: Target {
                width: 0,
                height: 0,
                scale: 1.0,
                rounding: 0.0,
            },
            active: false,
            frames: 0,
        })
    }

    /// Show the glow and start the animation if the scene has something to show.
    fn wake(&mut self) {
        let now = Instant::now();
        if self.active || !self.scene.on_screen(now) {
            return;
        }
        let _dpi = DpiAware::new();
        (self.origin, self.target) = unsafe { glow_target() };
        self.scene.tick(now, crate::recorder::take_level());
        self.present();
        unsafe {
            SetTimer(self.windows[0].hwnd, GLOW_TIMER_ID, FRAME_MS, None);
        }
        self.active = true;
        self.frames = 0;
    }

    fn frame(&mut self) {
        let _dpi = DpiAware::new();
        let now = Instant::now();
        self.scene.tick(now, crate::recorder::take_level());
        if self.scene.on_screen(now) {
            self.frames += 1;
            if self.frames.is_multiple_of(FOLLOW_FRAMES) {
                (self.origin, self.target) = unsafe { glow_target() };
            }
            self.present();
            return;
        }
        unsafe {
            KillTimer(self.windows[0].hwnd, GLOW_TIMER_ID);
        }
        for window in &mut self.windows {
            window.hide();
        }
        self.active = false;
    }

    /// Draw the frame into each piece and put it on screen.
    fn present(&mut self) {
        let frame = self.scene.frame();
        for (window, piece) in self.windows.iter_mut().zip(Piece::ALL) {
            let (x, y, w, h) = self.target.rect(piece);
            if w <= 0 || h <= 0 {
                window.hide();
                continue;
            }
            if window
                .surface
                .as_ref()
                .is_none_or(|s| (s.width, s.height) != (w, h))
            {
                window.surface = Surface::new(w, h);
            }
            let Some(surface) = &mut window.surface else {
                window.hide();
                continue;
            };
            glow_paint::draw(surface.pixels(), &self.target, piece, &frame);
            let origin = POINT {
                x: self.origin.x + x,
                y: self.origin.y + y,
            };
            if surface.present(window.hwnd, origin) && !window.shown {
                show(window.hwnd);
                window.shown = true;
            }
        }
    }

    fn destroy(self) {
        unsafe {
            KillTimer(self.windows[0].hwnd, GLOW_TIMER_ID);
        }
        for window in self.windows {
            unsafe { DestroyWindow(window.hwnd) };
        }
    }
}

/// The overlay, owned by the UI thread. The windows and their state live in thread-locals, where the window
/// procedure finds them.
#[derive(Default)]
pub struct Overlay(());

impl Overlay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show the configured style; the other one gets a disabled status, so it hides (or stays hidden). If the
    /// edge glow can't be shown, the pill is used instead.
    pub fn set_status(&mut self, status: OverlayStatus) {
        let off = OverlayStatus {
            enabled: false,
            ..status
        };
        let want_glow = status.enabled && status.style == OverlayStyle::EdgeGlow;
        let glow = if want_glow || glow_exists() {
            with_glow(|glow| {
                glow.scene.set_status(if want_glow { status } else { off });
                glow.wake();
            }) && want_glow
        } else {
            false
        };
        let pill = if glow { off } else { status };
        let exists = STATE.with(|cell| cell.try_borrow().is_ok_and(|s| s.is_some()));
        if !pill.enabled && !exists {
            return; // no window until it's needed
        }
        with_state(|state| {
            state.scene.set_status(pill);
            state.wake();
        });
    }

    /// The style that isn't shown has a disabled status, so only the shown one reacts.
    pub fn feedback(&mut self, feedback: Feedback) {
        let now = Instant::now();
        STATE.with(|cell| {
            if let Ok(mut slot) = cell.try_borrow_mut()
                && let Some(state) = slot.as_mut()
            {
                state.scene.feedback(feedback, now);
                state.wake();
            }
        });
        GLOW.with(|cell| {
            if let Ok(mut slot) = cell.try_borrow_mut()
                && let Some(glow) = slot.as_mut()
            {
                glow.scene.feedback(feedback, now);
                glow.wake();
            }
        });
    }

    pub fn destroy(&mut self) {
        if let Some(state) =
            STATE.with(|cell| cell.try_borrow_mut().ok().and_then(|mut s| s.take()))
        {
            unsafe {
                KillTimer(state.hwnd, TIMER_ID);
                DestroyWindow(state.hwnd);
            }
        }
        if let Some(glow) = GLOW.with(|cell| cell.try_borrow_mut().ok().and_then(|mut s| s.take()))
        {
            glow.destroy();
        }
    }
}

fn glow_exists() -> bool {
    GLOW.with(|cell| cell.try_borrow().is_ok_and(|s| s.is_some()))
}
